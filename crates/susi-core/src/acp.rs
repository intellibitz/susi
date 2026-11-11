//! Agent Client Protocol / Agent Communication Protocol (ACP) open-socket pillar.
//!
//! `susi_core::acp` provides Susi Core's Apache-2.0 standard ACP surface over
//! both HTTP/1.1 (`GET /.well-known/acp.json`, `POST /acp`, `POST /acp/stream`)
//! and newline-delimited JSON-RPC 2.0 over stdio (`AcpStdioTransport`), with:
//! - protocol version negotiation (`initialize` / `acp/initialize`),
//! - public capability discovery (`/.well-known/acp.json` & `acp/discover`),
//! - bounded stateful session lifecycle (`session/new`, `session/load`,
//!   `session/list`, `session/close`),
//! - streaming prompt turns (`session/prompt` / `acp/turn/start`) with
//!   deterministic SHA-256 turn receipts,
//! - cooperative atomic turn cancellation (`session/cancel` / `acp/turn/cancel`),
//! - Mandatory Access Control (`MacPolicy`) capability & privacy admission
//!   before session creation or prompt execution, and
//! - zero third-party HTTP/RPC dependencies (`std::net` + `plain_http`).

use crate::mac_policy::{self, MacPolicy};
use crate::plain_http;
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use sha2::{Digest, Sha256};
use std::collections::{BTreeMap, VecDeque};
use std::io::{self, BufRead, BufReader, Read, Write};
use std::net::{IpAddr, Ipv4Addr, Shutdown, SocketAddr, TcpListener, TcpStream};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

/// Discovery schema identifier for `GET /.well-known/acp.json`.
pub const ACP_DISCOVERY_SCHEMA: &str = "susi-acp-discovery/v1";
/// Canonical ACP wire protocol version.
pub const ACP_PROTOCOL_VERSION: &str = "2026-11-11";
/// Backwards-compatible ACP protocol alias accepted during `initialize`.
pub const ACP_PROTOCOL_VERSION_V1: &str = "1.0";
/// Public discovery path mounted alongside A2A on port `9094 + port_offset`.
pub const WELL_KNOWN_ACP_PATH: &str = "/.well-known/acp.json";
/// Authenticated JSON-RPC 2.0 endpoint path.
pub const ACP_RPC_PATH: &str = "/acp";
/// Authenticated Server-Sent Events (SSE) turn stream endpoint path.
pub const ACP_STREAM_PATH: &str = "/acp/stream";
/// Public health probe path.
pub const ACP_HEALTH_PATH: &str = "/health";
/// Canonical host-contract HTTP port shared with A2A (`9094 + port_offset`).
pub const ACP_CANONICAL_PORT: u16 = 9094;

/// JSON-RPC 2.0 standard error codes and ACP domain refusal codes.
pub const JSONRPC_PARSE_ERROR: i64 = -32700;
pub const JSONRPC_INVALID_REQUEST: i64 = -32600;
pub const JSONRPC_METHOD_NOT_FOUND: i64 = -32601;
pub const JSONRPC_INVALID_PARAMS: i64 = -32602;
pub const JSONRPC_INTERNAL_ERROR: i64 = -32603;
pub const ACP_ERR_NOT_FOUND: i64 = -32001;
pub const ACP_ERR_CAPABILITY_REFUSED: i64 = -32003;

const DEFAULT_MAX_SESSIONS: usize = 256;
const DEFAULT_MAX_PROMPT_BYTES: usize = 512 * 1024;
const MAX_HTTP_HEAD_BYTES: usize = 32 * 1024;
const MAX_HTTP_HEADERS: usize = 64;
const MAX_HTTP_BODY_BYTES: usize = 1024 * 1024;
const MAX_STDIO_LINE_BYTES: usize = 1024 * 1024;

/// Baseline ACP capabilities always permitted on an authenticated socket
/// unless blocked by explicit workspace traversal checks.
const BASELINE_ACP_CAPABILITIES: &[&str] = &[
    "acp.session",
    "acp.prompt",
    "acp.stream",
    "acp.cancel",
    "mcp.read",
];

/// Discovery document served at `GET /.well-known/acp.json` and `acp/discover`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct AcpDiscoveryDocument {
    pub schema: String,
    pub protocol_version: String,
    pub compatible_versions: Vec<String>,
    pub agent: AcpAgentInfo,
    pub endpoints: AcpEndpoints,
    pub auth: AcpAuthScheme,
    pub capabilities: AcpCapabilities,
}

/// Agent identity advertised in discovery and `initialize`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct AcpAgentInfo {
    pub id: String,
    pub name: String,
    pub version: String,
    pub license: String,
    pub canonical_port: u16,
}

/// Transport endpoints advertised by the ACP pillar.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct AcpEndpoints {
    pub discovery: String,
    pub rpc: String,
    pub stream: String,
    pub stdio: String,
}

/// Authentication requirement advertised for non-discovery ACP routes.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct AcpAuthScheme {
    pub scheme: String,
    pub header: String,
    pub realm: String,
}

/// Feature flags advertised by the ACP pillar.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct AcpCapabilities {
    pub sessions: bool,
    pub streaming: bool,
    pub cancellation: bool,
    pub capability_admission: bool,
    pub mcp_bridge: bool,
}

impl Default for AcpCapabilities {
    fn default() -> Self {
        Self {
            sessions: true,
            streaming: true,
            cancellation: true,
            capability_admission: true,
            mcp_bridge: true,
        }
    }
}

/// Reason a prompt turn terminated.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum AcpStopReason {
    EndTurn,
    Cancelled,
    Refused,
    Failed,
}

impl AcpStopReason {
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::EndTurn => "end_turn",
            Self::Cancelled => "cancelled",
            Self::Refused => "refused",
            Self::Failed => "failed",
        }
    }
}

/// Structured content block carried in ACP prompts and turn outputs.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct AcpContentBlock {
    #[serde(rename = "type")]
    pub kind: String,
    pub text: String,
}

impl AcpContentBlock {
    #[must_use]
    pub fn text(value: impl Into<String>) -> Self {
        Self {
            kind: "text".to_owned(),
            text: value.into(),
        }
    }
}

/// Ordered event emitted during an ACP prompt turn (returned in JSON-RPC and
/// streamed live as `event: acp.turn.event` SSE frames on `/acp/stream`).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct AcpTurnEvent {
    pub seq: u64,
    pub session_id: String,
    pub turn_id: String,
    pub kind: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub delta: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub stop_reason: Option<AcpStopReason>,
}

/// Completed or cancelled turn outcome returned from `session/prompt`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct AcpTurnResult {
    pub session_id: String,
    pub turn_id: String,
    pub stop_reason: AcpStopReason,
    pub output: Vec<AcpContentBlock>,
    pub events: Vec<AcpTurnEvent>,
    pub receipt_sha256: String,
}

/// Summary of an ACP session tracked by [`AcpEngine`].
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct AcpSessionSummary {
    pub session_id: String,
    pub protocol_version: String,
    pub workspace: Option<String>,
    pub admitted_capabilities: Vec<String>,
    pub created_unix_ms: u64,
    pub turns_completed: u64,
    pub active_turn_id: Option<String>,
}

/// Execution context passed to an [`AcpTurnExecutor`].
#[derive(Debug, Clone)]
pub struct AcpTurnContext {
    pub session_id: String,
    pub turn_id: String,
    pub workspace: Option<String>,
    pub admitted_capabilities: Vec<String>,
    pub prompt: Vec<AcpContentBlock>,
}

impl AcpTurnContext {
    /// Concatenated text blocks in `prompt`, separated by newlines.
    #[must_use]
    pub fn joined_prompt_text(&self) -> String {
        let mut out = String::new();
        for block in &self.prompt {
            if block.kind == "text" {
                if !out.is_empty() {
                    out.push('\n');
                }
                out.push_str(&block.text);
            }
        }
        out
    }
}

/// Pluggable turn execution strategy behind [`AcpEngine`].
pub trait AcpTurnExecutor: Send + Sync {
    fn execute_turn(
        &self,
        ctx: &AcpTurnContext,
        cancelled: &AtomicBool,
        emit_delta: &mut dyn FnMut(&str),
    ) -> Result<Vec<AcpContentBlock>, String>;
}

/// Default zero-bloat deterministic ACP substrate executor.
#[derive(Debug, Default)]
pub struct SubstrateAcpExecutor;

impl AcpTurnExecutor for SubstrateAcpExecutor {
    fn execute_turn(
        &self,
        ctx: &AcpTurnContext,
        cancelled: &AtomicBool,
        emit_delta: &mut dyn FnMut(&str),
    ) -> Result<Vec<AcpContentBlock>, String> {
        if cancelled.load(Ordering::SeqCst) {
            return Ok(Vec::new());
        }
        let prompt_text = ctx.joined_prompt_text();
        let summary = format!("acp-ack[{}]: {}", ctx.session_id, prompt_text.trim());
        emit_delta(&summary);
        Ok(vec![AcpContentBlock::text(summary)])
    }
}

/// Admission configuration binding ACP sessions and turns to [`MacPolicy`].
#[derive(Clone)]
pub struct AcpAdmissionPolicy {
    pub mac_policy: Option<Arc<MacPolicy>>,
    pub subject: String,
    pub max_sessions: usize,
    pub max_prompt_bytes: usize,
}

impl Default for AcpAdmissionPolicy {
    fn default() -> Self {
        Self {
            mac_policy: None,
            subject: "acp-client".to_owned(),
            max_sessions: DEFAULT_MAX_SESSIONS,
            max_prompt_bytes: DEFAULT_MAX_PROMPT_BYTES,
        }
    }
}

impl AcpAdmissionPolicy {
    #[must_use]
    pub fn with_mac_policy(mac_policy: Arc<MacPolicy>, subject: impl Into<String>) -> Self {
        Self {
            mac_policy: Some(mac_policy),
            subject: subject.into(),
            max_sessions: DEFAULT_MAX_SESSIONS,
            max_prompt_bytes: DEFAULT_MAX_PROMPT_BYTES,
        }
    }

    fn policy(&self) -> &MacPolicy {
        match &self.mac_policy {
            Some(custom) => custom.as_ref(),
            None => MacPolicy::global(),
        }
    }

    fn validate_workspace(workspace: Option<&str>) -> Result<Option<String>, (i64, String)> {
        let Some(raw) = workspace else {
            return Ok(None);
        };
        let trimmed = raw.trim();
        if trimmed.is_empty() {
            return Ok(None);
        }
        if trimmed.chars().any(char::is_control)
            || trimmed.replace('\\', "/").split('/').any(|seg| seg == "..")
        {
            return Err((
                ACP_ERR_CAPABILITY_REFUSED,
                format!("workspace path rejected by confinement guard: {trimmed}"),
            ));
        }
        Ok(Some(trimmed.to_owned()))
    }

    fn check_capabilities(
        &self,
        requested: &[String],
        resource: &str,
    ) -> Result<Vec<String>, (i64, String)> {
        let policy = self.policy();
        let mode = policy.mode();
        let mut admitted = Vec::with_capacity(requested.len().max(1));
        for cap in requested {
            let clean = cap.trim();
            if clean.is_empty() {
                continue;
            }
            if (clean == mac_policy::actions::NETWORK_EGRESS
                || clean == mac_policy::actions::CLOUD_INFERENCE)
                && mode.confines_to_host()
            {
                return Err((
                    ACP_ERR_CAPABILITY_REFUSED,
                    format!(
                        "capability '{clean}' refused: privacy mode '{}' confines traffic to loopback",
                        mode.as_str()
                    ),
                ));
            }
            if (clean == mac_policy::actions::NETWORK_EGRESS
                || clean == mac_policy::actions::CLOUD_INFERENCE)
                && mode.restricts_egress()
                && !policy.is_permitted(&self.subject, clean, resource)
            {
                return Err((
                    ACP_ERR_CAPABILITY_REFUSED,
                    format!(
                        "capability '{clean}' refused under privacy mode '{}' without explicit MAC grant",
                        mode.as_str()
                    ),
                ));
            }
            let baseline = BASELINE_ACP_CAPABILITIES.contains(&clean);
            if !baseline && !policy.is_permitted(&self.subject, clean, resource) {
                return Err((
                    ACP_ERR_CAPABILITY_REFUSED,
                    format!(
                        "capability '{clean}' refused by MacPolicy for subject '{}' on '{resource}'",
                        self.subject
                    ),
                ));
            }
            if !admitted.iter().any(|existing| existing == clean) {
                admitted.push(clean.to_owned());
            }
        }
        if admitted.is_empty() {
            admitted.push("acp.session".to_owned());
            admitted.push("acp.prompt".to_owned());
        }
        Ok(admitted)
    }
}

struct SessionEntry {
    summary: AcpSessionSummary,
    metadata: Value,
    pre_cancelled: bool,
}

#[derive(Default)]
struct EngineState {
    order: VecDeque<String>,
    sessions: BTreeMap<String, SessionEntry>,
    active_cancellations: BTreeMap<String, Arc<AtomicBool>>,
    turn_cancellations: BTreeMap<(String, String), Arc<AtomicBool>>,
}

/// Core stateful ACP protocol engine shared across HTTP, A2A port 9094, and stdio.
pub struct AcpEngine {
    admission: AcpAdmissionPolicy,
    executor: Arc<dyn AcpTurnExecutor>,
    engine_version: String,
    canonical_port: u16,
    next_id: AtomicU64,
    state: Mutex<EngineState>,
}

impl Default for AcpEngine {
    fn default() -> Self {
        Self::new(
            AcpAdmissionPolicy::default(),
            Arc::new(SubstrateAcpExecutor),
        )
    }
}

impl AcpEngine {
    #[must_use]
    pub fn new(admission: AcpAdmissionPolicy, executor: Arc<dyn AcpTurnExecutor>) -> Self {
        Self {
            admission,
            executor,
            engine_version: env!("CARGO_PKG_VERSION").to_owned(),
            canonical_port: ACP_CANONICAL_PORT,
            next_id: AtomicU64::new(1),
            state: Mutex::new(EngineState::default()),
        }
    }

    #[must_use]
    pub fn discovery_document(&self) -> AcpDiscoveryDocument {
        AcpDiscoveryDocument {
            schema: ACP_DISCOVERY_SCHEMA.to_owned(),
            protocol_version: ACP_PROTOCOL_VERSION.to_owned(),
            compatible_versions: vec![
                ACP_PROTOCOL_VERSION.to_owned(),
                ACP_PROTOCOL_VERSION_V1.to_owned(),
            ],
            agent: AcpAgentInfo {
                id: "susi-core".to_owned(),
                name: "Susi Core ACP Pillar".to_owned(),
                version: self.engine_version.clone(),
                license: "Apache-2.0".to_owned(),
                canonical_port: self.canonical_port,
            },
            endpoints: AcpEndpoints {
                discovery: WELL_KNOWN_ACP_PATH.to_owned(),
                rpc: ACP_RPC_PATH.to_owned(),
                stream: ACP_STREAM_PATH.to_owned(),
                stdio: "ndjson".to_owned(),
            },
            auth: AcpAuthScheme {
                scheme: "Bearer".to_owned(),
                header: "Authorization".to_owned(),
                realm: "susi-a2a".to_owned(),
            },
            capabilities: AcpCapabilities::default(),
        }
    }

    /// Dispatch a raw JSON-RPC 2.0 request body and return a serialized JSON-RPC 2.0 response.
    #[must_use]
    pub fn handle_jsonrpc_bytes(&self, body: &[u8]) -> Vec<u8> {
        let parsed: Value = match serde_json::from_slice(body) {
            Ok(val) => val,
            Err(err) => {
                return serialize_rpc_response(&rpc_error(
                    Value::Null,
                    JSONRPC_PARSE_ERROR,
                    format!("Parse error: {err}"),
                ));
            }
        };
        let reply = self.handle_jsonrpc_value(parsed, &mut |_| {});
        serialize_rpc_response(&reply)
    }

    /// Dispatch a raw JSON-RPC 2.0 request body while streaming SSE frames via `on_sse_frame`.
    ///
    /// If the request is not a valid `session/prompt` (`acp/turn/start`), returns `Err(json_bytes)`
    /// so the HTTP route can return an immediate `application/json` JSON-RPC error response.
    pub fn handle_sse_stream_bytes(&self, body: &[u8]) -> Result<Vec<String>, Vec<u8>> {
        let parsed: Value = match serde_json::from_slice(body) {
            Ok(val) => val,
            Err(err) => {
                return Err(serialize_rpc_response(&rpc_error(
                    Value::Null,
                    JSONRPC_PARSE_ERROR,
                    format!("Parse error: {err}"),
                )));
            }
        };
        let (id, method, params) = match validate_rpc_envelope(&parsed) {
            Ok(parts) => parts,
            Err(err_val) => return Err(serialize_rpc_response(&err_val)),
        };
        if !matches!(method.as_str(), "session/prompt" | "acp/turn/start") {
            return Err(serialize_rpc_response(&rpc_error(
                id,
                JSONRPC_METHOD_NOT_FOUND,
                format!("Method '{method}' is not supported on SSE stream endpoint"),
            )));
        }
        let mut frames = Vec::new();
        match self.dispatch_prompt(&params, &mut |event| {
            if let Ok(encoded) = serde_json::to_string(&event) {
                frames.push(format!("event: acp.turn.event\ndata: {encoded}\n\n"));
            }
        }) {
            Ok(result_val) => {
                let envelope = rpc_ok(id, result_val);
                if let Ok(encoded) = serde_json::to_string(&envelope) {
                    frames.push(format!("event: acp.turn.result\ndata: {encoded}\n\n"));
                }
                Ok(frames)
            }
            Err((code, msg)) => Err(serialize_rpc_response(&rpc_error(id, code, msg))),
        }
    }

    /// Dispatch a parsed JSON-RPC 2.0 value and return the JSON-RPC 2.0 reply object.
    pub fn handle_jsonrpc_value(
        &self,
        request: Value,
        on_event: &mut dyn FnMut(AcpTurnEvent),
    ) -> Value {
        let (id, method, params) = match validate_rpc_envelope(&request) {
            Ok(parts) => parts,
            Err(err_val) => return err_val,
        };
        let outcome = match method.as_str() {
            "initialize" | "acp/initialize" => self.dispatch_initialize(&params),
            "acp/discover" | "agent/discover" => serde_json::to_value(self.discovery_document())
                .map_err(|e| (JSONRPC_INTERNAL_ERROR, e.to_string())),
            "session/new" | "acp/session/create" => self.dispatch_session_new(&params),
            "session/load" | "acp/session/get" => self.dispatch_session_load(&params),
            "session/list" | "acp/session/list" => self.dispatch_session_list(),
            "session/close" | "acp/session/close" => self.dispatch_session_close(&params),
            "session/prompt" | "acp/turn/start" => self.dispatch_prompt(&params, on_event),
            "session/cancel" | "acp/turn/cancel" => self.dispatch_cancel(&params),
            other => Err((
                JSONRPC_METHOD_NOT_FOUND,
                format!("Method '{other}' not found"),
            )),
        };
        match outcome {
            Ok(result) => rpc_ok(id, result),
            Err((code, message)) => rpc_error(id, code, message),
        }
    }

    fn dispatch_initialize(&self, params: &Value) -> Result<Value, (i64, String)> {
        let requested_version = params
            .get("protocol_version")
            .or_else(|| params.get("protocolVersion"))
            .and_then(Value::as_str)
            .unwrap_or(ACP_PROTOCOL_VERSION);
        if requested_version != ACP_PROTOCOL_VERSION && requested_version != ACP_PROTOCOL_VERSION_V1
        {
            return Err((
                JSONRPC_INVALID_PARAMS,
                format!(
                    "Unsupported ACP protocol_version '{requested_version}'; supported: {ACP_PROTOCOL_VERSION}, {ACP_PROTOCOL_VERSION_V1}"
                ),
            ));
        }
        let doc = self.discovery_document();
        Ok(json!({
            "protocol_version": ACP_PROTOCOL_VERSION,
            "requested_protocol_version": requested_version,
            "agent_info": doc.agent,
            "capabilities": doc.capabilities,
            "endpoints": doc.endpoints,
        }))
    }

    fn dispatch_session_new(&self, params: &Value) -> Result<Value, (i64, String)> {
        let workspace = AcpAdmissionPolicy::validate_workspace(
            params.get("workspace").and_then(Value::as_str),
        )?;
        let requested_caps = extract_string_list(params.get("capabilities"))?;
        let resource = workspace.as_deref().unwrap_or("*");
        let admitted_capabilities = self
            .admission
            .check_capabilities(&requested_caps, resource)?;
        let metadata = params.get("metadata").cloned().unwrap_or_else(|| json!({}));

        let seq = self.next_id.fetch_add(1, Ordering::Relaxed);
        let session_id = format!("acp-sess-{seq:06}");
        let summary = AcpSessionSummary {
            session_id: session_id.clone(),
            protocol_version: ACP_PROTOCOL_VERSION.to_owned(),
            workspace,
            admitted_capabilities,
            created_unix_ms: now_unix_ms(),
            turns_completed: 0,
            active_turn_id: None,
        };

        let mut guard = self.state.lock().unwrap_or_else(|e| e.into_inner());
        while guard.sessions.len() >= self.admission.max_sessions {
            let Some(oldest) = guard.order.pop_front() else {
                break;
            };
            let is_busy = guard
                .sessions
                .get(&oldest)
                .is_some_and(|s| s.summary.active_turn_id.is_some());
            if is_busy {
                guard.order.push_back(oldest);
                return Err((
                    ACP_ERR_CAPABILITY_REFUSED,
                    "maximum concurrent active ACP sessions reached".to_owned(),
                ));
            }
            guard.sessions.remove(&oldest);
            guard.active_cancellations.remove(&oldest);
        }
        guard.order.push_back(session_id.clone());
        guard.sessions.insert(
            session_id,
            SessionEntry {
                summary: summary.clone(),
                metadata,
                pre_cancelled: false,
            },
        );
        serde_json::to_value(summary).map_err(|e| (JSONRPC_INTERNAL_ERROR, e.to_string()))
    }

    fn dispatch_session_load(&self, params: &Value) -> Result<Value, (i64, String)> {
        let session_id = required_str(params, "session_id")?;
        let guard = self.state.lock().unwrap_or_else(|e| e.into_inner());
        let entry = guard.sessions.get(session_id).ok_or_else(|| {
            (
                ACP_ERR_NOT_FOUND,
                format!("ACP session not found: {session_id}"),
            )
        })?;
        Ok(json!({
            "session": entry.summary,
            "metadata": entry.metadata,
        }))
    }

    fn dispatch_session_list(&self) -> Result<Value, (i64, String)> {
        let guard = self.state.lock().unwrap_or_else(|e| e.into_inner());
        let list: Vec<&AcpSessionSummary> = guard
            .order
            .iter()
            .filter_map(|id| guard.sessions.get(id).map(|entry| &entry.summary))
            .collect();
        Ok(json!({ "sessions": list }))
    }

    fn dispatch_session_close(&self, params: &Value) -> Result<Value, (i64, String)> {
        let session_id = required_str(params, "session_id")?;
        let mut guard = self.state.lock().unwrap_or_else(|e| e.into_inner());
        if guard.sessions.remove(session_id).is_none() {
            return Err((
                ACP_ERR_NOT_FOUND,
                format!("ACP session not found: {session_id}"),
            ));
        }
        guard.order.retain(|item| item != session_id);
        if let Some(cancel_flag) = guard.active_cancellations.remove(session_id) {
            cancel_flag.store(true, Ordering::SeqCst);
        }
        guard
            .turn_cancellations
            .retain(|(sid, _), _| sid != session_id);
        Ok(json!({
            "session_id": session_id,
            "closed": true,
        }))
    }

    fn dispatch_cancel(&self, params: &Value) -> Result<Value, (i64, String)> {
        let session_id = required_str(params, "session_id")?;
        let turn_id = params
            .get("turn_id")
            .and_then(Value::as_str)
            .map(str::trim)
            .filter(|s| !s.is_empty());

        let mut guard = self.state.lock().unwrap_or_else(|e| e.into_inner());
        if !guard.sessions.contains_key(session_id) {
            return Err((
                ACP_ERR_NOT_FOUND,
                format!("ACP session not found: {session_id}"),
            ));
        }
        let mut signalled = false;
        if let Some(tid) = turn_id {
            let key = (session_id.to_owned(), tid.to_owned());
            if let Some(flag) = guard.turn_cancellations.get(&key) {
                flag.store(true, Ordering::SeqCst);
                signalled = true;
            } else {
                let flag = Arc::new(AtomicBool::new(true));
                guard.turn_cancellations.insert(key, flag);
                signalled = true;
            }
        } else {
            if let Some(flag) = guard.active_cancellations.get(session_id) {
                flag.store(true, Ordering::SeqCst);
                signalled = true;
            }
            if let Some(entry) = guard.sessions.get_mut(session_id) {
                if !signalled {
                    entry.pre_cancelled = true;
                    signalled = true;
                }
            }
        }
        Ok(json!({
            "session_id": session_id,
            "turn_id": turn_id,
            "cancelled": signalled,
            "stop_reason": AcpStopReason::Cancelled.as_str(),
        }))
    }

    fn dispatch_prompt(
        &self,
        params: &Value,
        on_event: &mut dyn FnMut(AcpTurnEvent),
    ) -> Result<Value, (i64, String)> {
        let session_id = required_str(params, "session_id")?.to_owned();
        let prompt_blocks =
            parse_prompt_blocks(params.get("prompt"), self.admission.max_prompt_bytes)?;
        let turn_caps = extract_string_list(params.get("requested_capabilities"))?;
        let explicit_turn_id = params
            .get("turn_id")
            .and_then(Value::as_str)
            .map(str::trim)
            .filter(|s| !s.is_empty())
            .map(str::to_owned);

        let turn_id = match explicit_turn_id {
            Some(tid) => tid,
            None => {
                let n = self.next_id.fetch_add(1, Ordering::Relaxed);
                format!("acp-turn-{n:06}")
            }
        };

        let (ctx, cancel_flag) = {
            let mut guard = self.state.lock().unwrap_or_else(|e| e.into_inner());
            let entry = guard.sessions.get_mut(&session_id).ok_or_else(|| {
                (
                    ACP_ERR_NOT_FOUND,
                    format!("ACP session not found: {session_id}"),
                )
            })?;
            let resource = entry.summary.workspace.as_deref().unwrap_or("*");
            let mut merged_caps = entry.summary.admitted_capabilities.clone();
            if !turn_caps.is_empty() {
                let verified = self.admission.check_capabilities(&turn_caps, resource)?;
                for cap in verified {
                    if !merged_caps.contains(&cap) {
                        merged_caps.push(cap);
                    }
                }
            }
            let was_pre_cancelled = entry.pre_cancelled;
            entry.pre_cancelled = false;
            entry.summary.active_turn_id = Some(turn_id.clone());
            let ctx = AcpTurnContext {
                session_id: session_id.clone(),
                turn_id: turn_id.clone(),
                workspace: entry.summary.workspace.clone(),
                admitted_capabilities: merged_caps,
                prompt: prompt_blocks,
            };

            let key = (session_id.clone(), turn_id.clone());
            let flag = match guard.turn_cancellations.get(&key) {
                Some(existing) => Arc::clone(existing),
                None => {
                    let fresh = Arc::new(AtomicBool::new(was_pre_cancelled));
                    guard.turn_cancellations.insert(key, Arc::clone(&fresh));
                    fresh
                }
            };
            if was_pre_cancelled {
                flag.store(true, Ordering::SeqCst);
            }
            guard
                .active_cancellations
                .insert(session_id.clone(), Arc::clone(&flag));
            (ctx, flag)
        };

        let mut seq = 1_u64;
        let mut events = Vec::new();
        let mut record_event = |kind: &str, delta: Option<String>, stop: Option<AcpStopReason>| {
            let ev = AcpTurnEvent {
                seq,
                session_id: session_id.clone(),
                turn_id: turn_id.clone(),
                kind: kind.to_owned(),
                delta,
                stop_reason: stop,
            };
            seq += 1;
            on_event(ev.clone());
            events.push(ev);
        };

        record_event("turn_started", None, None);

        let (stop_reason, output) = if cancel_flag.load(Ordering::SeqCst) {
            record_event("turn_cancelled", None, Some(AcpStopReason::Cancelled));
            (AcpStopReason::Cancelled, Vec::new())
        } else {
            let exec_res = self
                .executor
                .execute_turn(&ctx, &cancel_flag, &mut |chunk| {
                    if !cancel_flag.load(Ordering::SeqCst) {
                        record_event("message_delta", Some(chunk.to_owned()), None);
                    }
                });
            if cancel_flag.load(Ordering::SeqCst) {
                record_event("turn_cancelled", None, Some(AcpStopReason::Cancelled));
                (AcpStopReason::Cancelled, Vec::new())
            } else {
                match exec_res {
                    Ok(blocks) => {
                        record_event("turn_completed", None, Some(AcpStopReason::EndTurn));
                        (AcpStopReason::EndTurn, blocks)
                    }
                    Err(err_msg) => {
                        record_event(
                            "turn_failed",
                            Some(err_msg.clone()),
                            Some(AcpStopReason::Failed),
                        );
                        self.finish_turn_state(&session_id, &turn_id, false);
                        return Err((JSONRPC_INTERNAL_ERROR, err_msg));
                    }
                }
            }
        };

        self.finish_turn_state(&session_id, &turn_id, stop_reason == AcpStopReason::EndTurn);

        let receipt_sha256 = compute_turn_receipt(&ctx, stop_reason, &output);
        let result = AcpTurnResult {
            session_id,
            turn_id,
            stop_reason,
            output,
            events,
            receipt_sha256,
        };
        serde_json::to_value(result).map_err(|e| (JSONRPC_INTERNAL_ERROR, e.to_string()))
    }

    fn finish_turn_state(&self, session_id: &str, turn_id: &str, completed: bool) {
        let mut guard = self.state.lock().unwrap_or_else(|e| e.into_inner());
        if let Some(entry) = guard.sessions.get_mut(session_id) {
            if entry.summary.active_turn_id.as_deref() == Some(turn_id) {
                entry.summary.active_turn_id = None;
            }
            if completed {
                entry.summary.turns_completed = entry.summary.turns_completed.saturating_add(1);
            }
        }
        guard.active_cancellations.remove(session_id);
        guard
            .turn_cancellations
            .remove(&(session_id.to_owned(), turn_id.to_owned()));
    }
}

// ---------------------------------------------------------------------------
// Stdio NDJSON Transport
// ---------------------------------------------------------------------------

/// Newline-delimited JSON-RPC 2.0 ACP transport over `BufRead` + `Write`.
pub struct AcpStdioTransport;

impl AcpStdioTransport {
    /// Process a single NDJSON line and return the serialized JSON-RPC 2.0 response line.
    #[must_use]
    pub fn handle_line(engine: &AcpEngine, line: &str) -> Option<String> {
        let trimmed = line.trim();
        if trimmed.is_empty() {
            return None;
        }
        if trimmed.len() > MAX_STDIO_LINE_BYTES {
            let err = rpc_error(
                Value::Null,
                JSONRPC_INVALID_REQUEST,
                "ACP stdio frame exceeds maximum allowed size".to_owned(),
            );
            return serde_json::to_string(&err).ok();
        }
        let reply_bytes = engine.handle_jsonrpc_bytes(trimmed.as_bytes());
        Some(String::from_utf8_lossy(&reply_bytes).into_owned())
    }

    /// Serve NDJSON requests from `reader` and write one response line per request to `writer`.
    pub fn serve_reader_writer<R: BufRead, W: Write>(
        engine: &AcpEngine,
        mut reader: R,
        mut writer: W,
    ) -> io::Result<usize> {
        let mut handled = 0usize;
        let mut buf = String::new();
        loop {
            buf.clear();
            let read = reader.read_line(&mut buf)?;
            if read == 0 {
                break;
            }
            if let Some(reply) = Self::handle_line(engine, &buf) {
                writer.write_all(reply.as_bytes())?;
                writer.write_all(b"\n")?;
                writer.flush()?;
                handled = handled.saturating_add(1);
            }
        }
        Ok(handled)
    }
}

// ---------------------------------------------------------------------------
// Pure-std HTTP/1.1 Socket Server & Client
// ---------------------------------------------------------------------------

/// Handle for a running standalone HTTP/1.1 ACP socket server.
pub struct AcpSocketServer {
    addr: SocketAddr,
    shutdown: Arc<AtomicBool>,
    join: Option<std::thread::JoinHandle<()>>,
}

impl AcpSocketServer {
    /// Bind a standalone HTTP/1.1 ACP server on loopback (`127.0.0.1:0`) with
    /// mandatory Bearer token verification on non-discovery routes.
    pub fn bind_loopback(
        engine: Arc<AcpEngine>,
        bearer_token: impl Into<String>,
    ) -> io::Result<Self> {
        let listener = TcpListener::bind((Ipv4Addr::LOCALHOST, 0))?;
        Self::from_listener(listener, engine, bearer_token.into())
    }

    /// Serve an already-bound `TcpListener` on a background OS thread.
    pub fn from_listener(
        listener: TcpListener,
        engine: Arc<AcpEngine>,
        bearer_token: String,
    ) -> io::Result<Self> {
        let addr = listener.local_addr()?;
        listener.set_nonblocking(true)?;
        let shutdown = Arc::new(AtomicBool::new(false));
        let stop = Arc::clone(&shutdown);
        let token = Arc::new(bearer_token);
        let join = std::thread::Builder::new()
            .name("susi-acp-http".to_owned())
            .spawn(move || accept_loop(&listener, &engine, &token, &stop))?;
        Ok(Self {
            addr,
            shutdown,
            join: Some(join),
        })
    }

    #[must_use]
    pub const fn local_addr(&self) -> SocketAddr {
        self.addr
    }

    #[must_use]
    pub fn base_url(&self) -> String {
        format!("http://{}", self.addr)
    }

    pub fn shutdown(&mut self) {
        self.shutdown.store(true, Ordering::SeqCst);
        let _ = TcpStream::connect_timeout(&self.addr, Duration::from_millis(50));
        if let Some(handle) = self.join.take() {
            let _ = handle.join();
        }
    }
}

impl Drop for AcpSocketServer {
    fn drop(&mut self) {
        self.shutdown();
    }
}

fn accept_loop(
    listener: &TcpListener,
    engine: &Arc<AcpEngine>,
    bearer_token: &Arc<String>,
    shutdown: &AtomicBool,
) {
    while !shutdown.load(Ordering::SeqCst) {
        match listener.accept() {
            Ok((stream, peer)) => {
                if shutdown.load(Ordering::SeqCst) {
                    let _ = stream.shutdown(Shutdown::Both);
                    break;
                }
                if mac_policy::refuses_remote_client(peer.ip()) {
                    let _ = stream.shutdown(Shutdown::Both);
                    continue;
                }
                let eng = Arc::clone(engine);
                let tok = Arc::clone(bearer_token);
                let _ = std::thread::Builder::new()
                    .name("susi-acp-conn".to_owned())
                    .spawn(move || {
                        let _ = serve_http_connection(stream, peer.ip(), &eng, &tok);
                    });
            }
            Err(err) if err.kind() == io::ErrorKind::WouldBlock => {
                std::thread::sleep(Duration::from_millis(5));
            }
            Err(_) => {
                std::thread::sleep(Duration::from_millis(10));
            }
        }
    }
}

fn serve_http_connection(
    mut stream: TcpStream,
    _peer: IpAddr,
    engine: &AcpEngine,
    bearer_token: &str,
) -> io::Result<()> {
    stream.set_nonblocking(false)?;
    stream.set_read_timeout(Some(Duration::from_secs(15)))?;
    stream.set_write_timeout(Some(Duration::from_secs(15)))?;

    let (method, path, headers, body) = match read_http_request(&stream) {
        Ok(parts) => parts,
        Err(status) => {
            write_http_response(&mut stream, status, "text/plain", &[], b"")?;
            return Ok(());
        }
    };

    if method == "GET" && path == ACP_HEALTH_PATH {
        return write_http_response(
            &mut stream,
            200,
            "application/json",
            &[],
            br#"{"status":"ok"}"#,
        );
    }

    if method == "GET" && path == WELL_KNOWN_ACP_PATH {
        let payload = serde_json::to_vec(&engine.discovery_document()).unwrap_or_default();
        return write_http_response(&mut stream, 200, "application/json", &[], &payload);
    }

    // Every non-public route requires valid Bearer authorization.
    let auth_header = headers
        .iter()
        .find(|(k, _)| k.eq_ignore_ascii_case("authorization"))
        .map(|(_, v)| v.as_str());
    if !bearer_matches_expected(auth_header, bearer_token) {
        return write_http_response(
            &mut stream,
            401,
            "application/json",
            &[("WWW-Authenticate", "Bearer realm=\"susi-a2a\"")],
            b"",
        );
    }

    match (method.as_str(), path.as_str()) {
        ("POST", ACP_RPC_PATH) => {
            let reply = engine.handle_jsonrpc_bytes(&body);
            write_http_response(&mut stream, 200, "application/json", &[], &reply)
        }
        ("POST", ACP_STREAM_PATH) => match engine.handle_sse_stream_bytes(&body) {
            Ok(frames) => {
                let joined: String = frames.concat();
                write_http_response(
                    &mut stream,
                    200,
                    "text/event-stream",
                    &[("Cache-Control", "no-cache")],
                    joined.as_bytes(),
                )
            }
            Err(err_json) => {
                write_http_response(&mut stream, 200, "application/json", &[], &err_json)
            }
        },
        (_, ACP_RPC_PATH | ACP_STREAM_PATH) => {
            write_http_response(&mut stream, 405, "text/plain", &[("Allow", "POST")], b"")
        }
        (_, WELL_KNOWN_ACP_PATH | ACP_HEALTH_PATH) => {
            write_http_response(&mut stream, 405, "text/plain", &[("Allow", "GET")], b"")
        }
        _ => write_http_response(&mut stream, 404, "text/plain", &[], b""),
    }
}

type ParsedHttpRequest = (String, String, Vec<(String, String)>, Vec<u8>);

fn read_http_request(stream: &TcpStream) -> Result<ParsedHttpRequest, u16> {
    let cloned = stream.try_clone().map_err(|_| 400_u16)?;
    let mut reader = BufReader::new(cloned);
    let mut request_line = String::new();
    let mut head_bytes = 0usize;

    let n = reader
        .by_ref()
        .take(MAX_HTTP_HEAD_BYTES as u64)
        .read_line(&mut request_line)
        .map_err(|_| 400_u16)?;
    if n == 0 {
        return Err(400);
    }
    head_bytes = head_bytes.saturating_add(n);
    let mut parts = request_line
        .trim_end_matches(['\r', '\n'])
        .split_whitespace();
    let method = parts.next().ok_or(400_u16)?.to_owned();
    let raw_target = parts.next().ok_or(400_u16)?;
    let _version = parts.next().ok_or(400_u16)?;
    let path = raw_target
        .split('?')
        .next()
        .unwrap_or(raw_target)
        .to_owned();

    let mut headers = Vec::new();
    let mut content_length = 0usize;
    loop {
        let mut line = String::new();
        let read = reader
            .by_ref()
            .take((MAX_HTTP_HEAD_BYTES.saturating_sub(head_bytes) + 1) as u64)
            .read_line(&mut line)
            .map_err(|_| 400_u16)?;
        if read == 0 {
            return Err(400);
        }
        head_bytes = head_bytes.saturating_add(read);
        if head_bytes > MAX_HTTP_HEAD_BYTES || headers.len() >= MAX_HTTP_HEADERS {
            return Err(431);
        }
        let trimmed = line.trim_end_matches(['\r', '\n']);
        if trimmed.is_empty() {
            break;
        }
        let (name, value) = trimmed.split_once(':').ok_or(400_u16)?;
        let key = name.trim().to_owned();
        let val = value.trim().to_owned();
        if key.eq_ignore_ascii_case("content-length") {
            content_length = val.parse::<usize>().map_err(|_| 400_u16)?;
            if content_length > MAX_HTTP_BODY_BYTES {
                return Err(413);
            }
        }
        headers.push((key, val));
    }

    let mut body = vec![0_u8; content_length];
    if content_length > 0 {
        reader.read_exact(&mut body).map_err(|_| 400_u16)?;
    }
    Ok((method, path, headers, body))
}

fn write_http_response(
    stream: &mut TcpStream,
    status: u16,
    content_type: &str,
    extra_headers: &[(&str, &str)],
    body: &[u8],
) -> io::Result<()> {
    let reason = match status {
        200 => "OK",
        400 => "Bad Request",
        401 => "Unauthorized",
        404 => "Not Found",
        405 => "Method Not Allowed",
        413 => "Payload Too Large",
        431 => "Request Header Fields Too Large",
        _ => "Internal Server Error",
    };
    let mut head = format!(
        "HTTP/1.1 {status} {reason}\r\nContent-Type: {content_type}\r\nContent-Length: {}\r\nConnection: close\r\n",
        body.len()
    );
    for (k, v) in extra_headers {
        head.push_str(k);
        head.push_str(": ");
        head.push_str(v);
        head.push_str("\r\n");
    }
    head.push_str("\r\n");
    stream.write_all(head.as_bytes())?;
    if !body.is_empty() {
        stream.write_all(body)?;
    }
    stream.flush()
}

/// Decoded Server-Sent Event `(event_name, json_payload)` frame returned by `POST /acp/stream`.
pub type AcpSseFrame = (String, Value);

/// Blocking HTTP/1.1 ACP socket client built on `susi_core::plain_http`.
#[derive(Debug, Clone)]
pub struct AcpSocketClient {
    base_url: String,
    bearer_token: Option<String>,
    timeout_secs: u64,
}

impl AcpSocketClient {
    #[must_use]
    pub fn new(base_url: impl Into<String>, bearer_token: Option<String>) -> Self {
        Self {
            base_url: base_url.into().trim_end_matches('/').to_owned(),
            bearer_token,
            timeout_secs: 15,
        }
    }

    /// Fetch unauthenticated `GET /.well-known/acp.json`.
    pub fn discover(&self) -> Result<AcpDiscoveryDocument, String> {
        let url = format!("{}{WELL_KNOWN_ACP_PATH}", self.base_url);
        let resp = plain_http::call("GET", &url, &[], None, self.timeout_secs)?;
        if resp.status != 200 {
            return Err(format!("ACP discovery returned HTTP {}", resp.status));
        }
        let bytes = resp
            .into_bytes(MAX_HTTP_BODY_BYTES as u64)
            .map_err(|e| e.to_string())?;
        serde_json::from_slice(&bytes).map_err(|e| format!("invalid ACP discovery JSON: {e}"))
    }

    /// Send a raw JSON-RPC 2.0 request to `POST /acp` and return `(http_status, parsed_json)`.
    pub fn raw_rpc(&self, body: &Value) -> Result<(u16, Option<Value>), String> {
        let url = format!("{}{ACP_RPC_PATH}", self.base_url);
        let payload = serde_json::to_vec(body).map_err(|e| e.to_string())?;
        let auth_value = self
            .bearer_token
            .as_ref()
            .map(|tok| format!("Bearer {tok}"));
        let mut headers = vec![("Content-Type", "application/json")];
        if let Some(ref auth) = auth_value {
            headers.push(("Authorization", auth.as_str()));
        }
        let resp = plain_http::call("POST", &url, &headers, Some(&payload), self.timeout_secs)?;
        let status = resp.status;
        let bytes = resp
            .into_bytes(MAX_HTTP_BODY_BYTES as u64)
            .map_err(|e| e.to_string())?;
        let text = String::from_utf8(bytes).map_err(|e| e.to_string())?;
        if text.trim().is_empty() {
            return Ok((status, None));
        }
        let parsed = serde_json::from_str(&text)
            .map_err(|e| format!("invalid JSON-RPC response (HTTP {status}): {e}"))?;
        Ok((status, Some(parsed)))
    }

    /// Execute `session/prompt` over `POST /acp/stream` and return decoded SSE `(event_type, data_json)` frames.
    pub fn prompt_stream_sse(
        &self,
        id: u64,
        session_id: &str,
        prompt_text: &str,
    ) -> Result<(u16, Vec<AcpSseFrame>), String> {
        let url = format!("{}{ACP_STREAM_PATH}", self.base_url);
        let req = json!({
            "jsonrpc": "2.0",
            "id": id,
            "method": "session/prompt",
            "params": {
                "session_id": session_id,
                "prompt": [{ "type": "text", "text": prompt_text }],
            }
        });
        let payload = serde_json::to_vec(&req).map_err(|e| e.to_string())?;
        let auth_value = self
            .bearer_token
            .as_ref()
            .map(|tok| format!("Bearer {tok}"));
        let mut headers = vec![
            ("Content-Type", "application/json"),
            ("Accept", "text/event-stream"),
        ];
        if let Some(ref auth) = auth_value {
            headers.push(("Authorization", auth.as_str()));
        }
        let resp = plain_http::call("POST", &url, &headers, Some(&payload), self.timeout_secs)?;
        let status = resp.status;
        let bytes = resp
            .into_bytes(MAX_HTTP_BODY_BYTES as u64)
            .map_err(|e| e.to_string())?;
        let raw = String::from_utf8(bytes).map_err(|e| e.to_string())?;
        if status != 200 {
            return Ok((status, Vec::new()));
        }
        let mut events = Vec::new();
        for block in raw.split("\n\n").filter(|chunk| !chunk.trim().is_empty()) {
            let mut ev_name = "message".to_owned();
            let mut data_lines = Vec::new();
            for line in block.lines() {
                if let Some(rest) = line.strip_prefix("event:") {
                    ev_name = rest.trim().to_owned();
                } else if let Some(rest) = line.strip_prefix("data:") {
                    data_lines.push(rest.trim());
                }
            }
            if !data_lines.is_empty() {
                let joined = data_lines.join("\n");
                let val: Value = serde_json::from_str(&joined)
                    .map_err(|e| format!("malformed SSE JSON frame: {e}"))?;
                events.push((ev_name, val));
            }
        }
        Ok((status, events))
    }
}

// ---------------------------------------------------------------------------
// Internal JSON-RPC & Receipt Helpers
// ---------------------------------------------------------------------------

fn bearer_matches_expected(header: Option<&str>, expected_token: &str) -> bool {
    let expected = expected_token.trim().as_bytes();
    if expected.is_empty() {
        return false;
    }
    let Some(presented) = header.and_then(|h| h.strip_prefix("Bearer ")) else {
        return false;
    };
    let a = presented.trim().as_bytes();
    a.len() == expected.len()
        && a.iter()
            .zip(expected)
            .fold(0_u8, |acc, (x, y)| acc | (x ^ y))
            == 0
}

fn validate_rpc_envelope(request: &Value) -> Result<(Value, String, Value), Value> {
    let Some(obj) = request.as_object() else {
        return Err(rpc_error(
            Value::Null,
            JSONRPC_INVALID_REQUEST,
            "Invalid Request: JSON-RPC payload must be an object".to_owned(),
        ));
    };
    let id = obj.get("id").cloned().unwrap_or(Value::Null);
    if !matches!(id, Value::Null | Value::Number(_) | Value::String(_)) {
        return Err(rpc_error(
            Value::Null,
            JSONRPC_INVALID_REQUEST,
            "Invalid Request: 'id' must be string, number, or null".to_owned(),
        ));
    }
    if obj.get("jsonrpc").and_then(Value::as_str) != Some("2.0") {
        return Err(rpc_error(
            id,
            JSONRPC_INVALID_REQUEST,
            "Invalid Request: 'jsonrpc' must equal '2.0'".to_owned(),
        ));
    }
    let Some(method) = obj
        .get("method")
        .and_then(Value::as_str)
        .filter(|m| !m.trim().is_empty())
    else {
        return Err(rpc_error(
            id,
            JSONRPC_INVALID_REQUEST,
            "Invalid Request: missing non-empty string 'method'".to_owned(),
        ));
    };
    let params = match obj.get("params") {
        None | Some(Value::Null) => json!({}),
        Some(val @ Value::Object(_)) => val.clone(),
        Some(Value::Bool(_) | Value::Number(_) | Value::String(_) | Value::Array(_)) => {
            return Err(rpc_error(
                id,
                JSONRPC_INVALID_PARAMS,
                "Invalid params: 'params' must be a JSON object".to_owned(),
            ));
        }
    };
    Ok((id, method.to_owned(), params))
}

fn required_str<'a>(params: &'a Value, field: &str) -> Result<&'a str, (i64, String)> {
    params
        .get(field)
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|v| !v.is_empty())
        .ok_or_else(|| {
            (
                JSONRPC_INVALID_PARAMS,
                format!("Invalid params: missing non-empty field `{field}`"),
            )
        })
}

fn extract_string_list(field: Option<&Value>) -> Result<Vec<String>, (i64, String)> {
    match field {
        None | Some(Value::Null) => Ok(Vec::new()),
        Some(Value::Array(items)) => {
            let mut out = Vec::with_capacity(items.len());
            for item in items {
                let Some(text) = item.as_str() else {
                    return Err((
                        JSONRPC_INVALID_PARAMS,
                        "Invalid params: capability list items must be strings".to_owned(),
                    ));
                };
                out.push(text.to_owned());
            }
            Ok(out)
        }
        Some(Value::Bool(_) | Value::Number(_) | Value::String(_) | Value::Object(_)) => Err((
            JSONRPC_INVALID_PARAMS,
            "Invalid params: expected string array".to_owned(),
        )),
    }
}

fn parse_prompt_blocks(
    raw: Option<&Value>,
    max_prompt_bytes: usize,
) -> Result<Vec<AcpContentBlock>, (i64, String)> {
    let Some(val) = raw else {
        return Err((
            JSONRPC_INVALID_PARAMS,
            "Invalid params: missing field `prompt`".to_owned(),
        ));
    };
    let blocks = match val {
        Value::String(text) => vec![AcpContentBlock::text(text.clone())],
        Value::Array(items) => {
            if items.is_empty() {
                return Err((
                    JSONRPC_INVALID_PARAMS,
                    "Invalid params: `prompt` array must not be empty".to_owned(),
                ));
            }
            let mut parsed = Vec::with_capacity(items.len());
            for item in items {
                let block: AcpContentBlock = serde_json::from_value(item.clone()).map_err(|e| {
                    (
                        JSONRPC_INVALID_PARAMS,
                        format!("Invalid params: malformed prompt block: {e}"),
                    )
                })?;
                parsed.push(block);
            }
            parsed
        }
        Value::Null | Value::Bool(_) | Value::Number(_) | Value::Object(_) => {
            return Err((
                JSONRPC_INVALID_PARAMS,
                "Invalid params: `prompt` must be a string or content block array".to_owned(),
            ));
        }
    };
    let total_bytes: usize = blocks.iter().map(|b| b.text.len()).sum();
    if total_bytes > max_prompt_bytes {
        return Err((
            JSONRPC_INVALID_PARAMS,
            format!("Invalid params: prompt length {total_bytes} exceeds limit {max_prompt_bytes}"),
        ));
    }
    Ok(blocks)
}

fn compute_turn_receipt(
    ctx: &AcpTurnContext,
    stop_reason: AcpStopReason,
    output: &[AcpContentBlock],
) -> String {
    let canonical = json!({
        "schema": "susi-acp-turn-receipt/v1",
        "session_id": ctx.session_id,
        "turn_id": ctx.turn_id,
        "workspace": ctx.workspace,
        "admitted_capabilities": ctx.admitted_capabilities,
        "prompt": ctx.prompt,
        "stop_reason": stop_reason.as_str(),
        "output": output,
    });
    let bytes = serde_json::to_vec(&canonical).unwrap_or_default();
    let digest = Sha256::digest(&bytes);
    let mut hex = String::with_capacity(64);
    for byte in digest {
        use std::fmt::Write as _;
        let _ = write!(&mut hex, "{byte:02x}");
    }
    hex
}

fn now_unix_ms() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| u64::try_from(d.as_millis()).unwrap_or(u64::MAX))
        .unwrap_or(0)
}

fn rpc_ok(id: Value, result: Value) -> Value {
    json!({
        "jsonrpc": "2.0",
        "id": id,
        "result": result,
    })
}

fn rpc_error(id: Value, code: i64, message: String) -> Value {
    json!({
        "jsonrpc": "2.0",
        "id": id,
        "error": {
            "code": code,
            "message": message,
        }
    })
}

fn serialize_rpc_response(value: &Value) -> Vec<u8> {
    serde_json::to_vec(value).unwrap_or_else(|_| {
        br#"{"jsonrpc":"2.0","id":null,"error":{"code":-32603,"message":"Internal serialization error"}}"#
            .to_vec()
    })
}

#[cfg(test)]
#[path = "tests/acp_conformance.rs"]
mod tests;

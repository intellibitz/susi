#![forbid(unsafe_code)]
#![cfg_attr(
    test,
    allow(
        clippy::unwrap_used,
        clippy::expect_used,
        clippy::panic,
        clippy::unreachable,
        clippy::wildcard_enum_match_arm
    )
)]
//! Agent Client Protocol wire contracts.
//! Session, admission, executor, server and HTTP/SSE client live in Susi Commercial Apps.
use serde::{Deserialize, Serialize};
use serde_json::Value;
/// Discovery schema identifier for `GET /.well-known/acp.json`.
pub const ACP_DISCOVERY_SCHEMA: &str = "susi-acp-discovery/v1";
/// Legacy Susi ACP wire dialect version; official ACP uses an integer version.
pub const ACP_PROTOCOL_VERSION: &str = "2026-11-11";
/// Backwards-compatible legacy Susi ACP alias accepted during `initialize`.
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
/// Canonical HTTP session-binding header (`acp-session-id`).
pub const ACP_SESSION_ID_HEADER: &str = "acp-session-id";
/// JSON-RPC 2.0 standard error codes and ACP domain refusal codes.
pub const JSONRPC_PARSE_ERROR: i64 = -32700;
pub const JSONRPC_INVALID_REQUEST: i64 = -32600;
pub const JSONRPC_METHOD_NOT_FOUND: i64 = -32601;
pub const JSONRPC_INVALID_PARAMS: i64 = -32602;
pub const JSONRPC_INTERNAL_ERROR: i64 = -32603;
pub const ACP_ERR_NOT_FOUND: i64 = -32001;
pub const ACP_ERR_CAPABILITY_REFUSED: i64 = -32003;
/// Default maximum retained messages per ACP session ring buffer.
pub const DEFAULT_MAX_HISTORY_MESSAGES: usize = 64;
/// Default maximum retained message payload bytes per ACP session.
pub const DEFAULT_MAX_HISTORY_BYTES: usize = 512 * 1024;
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
fn default_true() -> bool {
    true
}
/// Feature flags advertised by the ACP pillar.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct AcpCapabilities {
    pub sessions: bool,
    pub streaming: bool,
    pub cancellation: bool,
    pub capability_admission: bool,
    pub mcp_bridge: bool,
    #[serde(default = "default_true")]
    pub session_history: bool,
}
impl Default for AcpCapabilities {
    fn default() -> Self {
        Self {
            sessions: true,
            streaming: true,
            cancellation: true,
            capability_admission: true,
            mcp_bridge: true,
            session_history: true,
        }
    }
}
/// Conversation participant role for multi-turn ACP session messages.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum AcpMessageRole {
    System,
    User,
    Assistant,
    Tool,
}
impl AcpMessageRole {
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::System => "system",
            Self::User => "user",
            Self::Assistant => "assistant",
            Self::Tool => "tool",
        }
    }
    pub fn from_wire_str(raw: &str) -> Result<Self, (i64, String)> {
        match raw.trim().to_ascii_lowercase().as_str() {
            "system" | "developer" => Ok(Self::System),
            "user" => Ok(Self::User),
            "assistant" | "agent" | "model" => Ok(Self::Assistant),
            "tool" => Ok(Self::Tool),
            other => Err((
                JSONRPC_INVALID_PARAMS,
                format!("Invalid params: unsupported ACP message role '{other}'"),
            )),
        }
    }
}
/// Structured tool invocation metadata attached to a multi-turn ACP message.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct AcpToolCallMeta {
    pub id: String,
    pub name: String,
    #[serde(default)]
    pub arguments: Value,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub status: Option<String>,
}
impl AcpToolCallMeta {
    #[must_use]
    pub fn approx_byte_len(&self) -> usize {
        let args_len = serde_json::to_vec(&self.arguments).map_or(0, |b| b.len());
        let status_len = match &self.status {
            Some(s) => s.len(),
            None => 0,
        };
        self.id
            .len()
            .saturating_add(self.name.len())
            .saturating_add(args_len)
            .saturating_add(status_len)
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
/// Bounded conversation message stored in an ACP session's multi-turn ring buffer.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct AcpSessionMessage {
    pub seq: u64,
    pub turn_id: String,
    pub role: AcpMessageRole,
    pub content: String,
    #[serde(default)]
    pub blocks: Vec<AcpContentBlock>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub tool_calls: Vec<AcpToolCallMeta>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub stop_reason: Option<AcpStopReason>,
    pub created_unix_ms: u64,
    pub byte_len: usize,
}
/// Compact multi-turn conversation message projection ready for downstream
/// model inference and commercial ACP plugs (`role`, `content`, `turn_id`, `tool_calls`).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct AcpInferenceMessage {
    pub role: AcpMessageRole,
    pub content: String,
    pub turn_id: String,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub tool_calls: Vec<AcpToolCallMeta>,
}
/// Per-session ring-buffer bounds and live occupancy telemetry.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct AcpHistoryBounds {
    pub max_messages: usize,
    pub max_bytes: usize,
    pub retained_messages: usize,
    pub retained_bytes: usize,
    pub evicted_messages: u64,
    pub oldest_seq: Option<u64>,
    pub newest_seq: Option<u64>,
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
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub tool_calls: Vec<AcpToolCallMeta>,
    pub events: Vec<AcpTurnEvent>,
    pub receipt_sha256: String,
    #[serde(default)]
    pub messages: Vec<AcpSessionMessage>,
    #[serde(default)]
    pub inference_messages: Vec<AcpInferenceMessage>,
    #[serde(default)]
    pub history_bounds: AcpHistoryBounds,
}
/// Summary of an ACP session reported by its implementation owner.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct AcpSessionSummary {
    pub session_id: String,
    pub protocol_version: String,
    pub workspace: Option<String>,
    pub admitted_capabilities: Vec<String>,
    pub created_unix_ms: u64,
    pub turns_completed: u64,
    #[serde(default)]
    pub turns_cancelled: u64,
    #[serde(default)]
    pub message_count: usize,
    #[serde(default)]
    pub history_bytes: usize,
    #[serde(default)]
    pub evicted_messages: u64,
    pub active_turn_id: Option<String>,
}
/// Official ACP v1 statuses. Cancellation uses Failed with explanatory content.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum AcpToolCallStatus {
    Pending,
    InProgress,
    Completed,
    Failed,
}
/// One invocation, identified uniquely within its session by the executor.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
#[non_exhaustive]
pub struct AcpToolCall {
    pub tool_call_id: String,
    pub title: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub name: Option<String>,
    pub status: AcpToolCallStatus,
}
impl AcpToolCall {
    pub fn new(id: impl Into<String>, title: impl Into<String>) -> Self {
        Self {
            tool_call_id: id.into(),
            title: title.into(),
            name: None,
            status: AcpToolCallStatus::Pending,
        }
    }
}
/// Partial update. An included content collection replaces previous content.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
#[non_exhaustive]
pub struct AcpToolCallUpdate {
    pub tool_call_id: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub status: Option<AcpToolCallStatus>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub content: Option<Vec<AcpToolCallContent>>,
}
impl AcpToolCallUpdate {
    pub fn new(id: impl Into<String>) -> Self {
        Self {
            tool_call_id: id.into(),
            status: None,
            content: None,
        }
    }
}
/// Display content for a tool invocation; it does not create execution evidence.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum AcpToolCallContent {
    Content { content: AcpContentBlock },
}
impl AcpToolCallContent {
    pub fn text(text: impl Into<String>) -> Self {
        Self::Content {
            content: AcpContentBlock::text(text),
        }
    }
}
/// Literal environment entry for an ACP-supplied stdio MCP server.
#[derive(Clone, Deserialize)]
pub struct AcpMcpEnvVar {
    pub name: String,
    pub value: String,
}
/// Validated stdio MCP descriptor. Values are not logged or stored in history.
#[derive(Clone, Deserialize)]
pub struct AcpMcpStdioServer {
    pub name: String,
    pub command: String,
    pub args: Vec<String>,
    pub env: Vec<AcpMcpEnvVar>,
}
/// External HTTP/SSE client handle; transport methods live in Susi Commercial Apps (`susi-gawd-acp`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AcpSocketClient {
    pub base_url: String,
}

#[cfg(test)]
#[path = "tests/protocol.rs"]
mod tests;

//! Canonical JSON-RPC wire gate for the GMCP stdio transport.
//!
//! rmcp's own codec drops whole classes of malformed input before a typed
//! answer can be produced — an unparseable line is ignored, an `id: null`
//! request deserializes into a notification and vanishes, and the
//! `2026-07-28` per-request `_meta` protocol fields are only enforced
//! inside individual handlers. This module is the single production path
//! that classifies every raw line *before* it reaches the service, per
//! `docs/gmcp/contract-203-002.md`:
//!
//! - unparseable JSON → `-32700` (parse error), never a silent drop;
//! - a JSON array → `-32600` (JSON-RPC batches are not defined by MCP);
//! - `id: null`, a missing/non-string method on an id-bearing message, or
//!   a non-`"2.0"` envelope → `-32600`;
//! - `_meta.io.modelcontextprotocol/protocolVersion` outside the
//!   supported set → `-32022` carrying `data.supported`/`data.requested`;
//! - a request on the modern path missing the required `_meta` protocol
//!   fields → `-32602`;
//! - outbound results are stamped with `resultType` when the payload
//!   does not already carry one (absent means `complete`).
//!
//! Everything else — typed dispatch, negotiation, correlation, typed
//! notifications — remains rmcp's: the gate forwards valid lines so one
//! implementation still serves every caller.

use serde_json::Value;
use std::future::Future;
use std::sync::Arc;
use susi_vendor_mcp_server as rmcp;
use tokio::io::{AsyncBufReadExt, AsyncRead, AsyncWrite, AsyncWriteExt, BufReader};
use tokio::sync::Mutex;

use rmcp::service::{RoleServer, RxJsonRpcMessage, TxJsonRpcMessage};
use rmcp::transport::Transport;

/// Protocol revisions the wire contract accepts on the modern path — the
/// `data.supported` list a `-32022` answer must carry. Source of truth for
/// both this gate and any negotiated-metadata reporting.
const MODERN_SUPPORTED_VERSIONS: &[&str] =
    &["2025-11-25", "2025-06-18", "2025-03-26", "2026-07-28"];

/// The `_meta` keys a request on the modern (`2026-07-28`) path MUST
/// carry; a request missing any of them is malformed (`-32602`).
const REQUIRED_MODERN_META_KEYS: &[&str] = &[
    "io.modelcontextprotocol/protocolVersion",
    "io.modelcontextprotocol/clientCapabilities",
];

const META_PROTOCOL_VERSION: &str = "io.modelcontextprotocol/protocolVersion";

/// Which era a stdio session has established. `2025-11-25` and earlier
/// negotiate the protocol version once inside `initialize`; the modern
/// path carries per-request `_meta` fields instead and is signalled by a
/// `server/discover` request or any request already carrying
/// `io.modelcontextprotocol/*` keys.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Era {
    Legacy,
    Modern,
}

/// The gate's decision for one raw line.
#[derive(Debug)]
pub enum GateVerdict {
    /// Valid envelope — hand the line to the service.
    Forward,
    /// Protocol-shaped input the compatibility rules deliberately ignore
    /// (e.g. a non-standard notification) — drop without a response.
    Drop,
    /// Malformed or policy-violating input — answer with this typed
    /// JSON-RPC error. `id` echoes the sender's request id when the
    /// envelope carried a legible one, else `null`.
    Reject {
        code: i64,
        message: &'static str,
        data: Option<Value>,
        id: Option<Value>,
    },
}

fn reject(code: i64, message: &'static str, data: Option<Value>, id: Option<Value>) -> GateVerdict {
    GateVerdict::Reject {
        code,
        message,
        data,
        id,
    }
}

/// Notifications the compatibility rules ignore outright — anything the
/// peer emits without an `id` that is not a standard MCP notification is
/// dropped rather than answered, matching rmcp's transport codec.
/// Methods a bare no-id message may name and still be ignored — the
/// same list rmcp's codec uses, so non-MCP notifications (LSP and
/// friends) stay compatibility-dropped rather than erroring.
fn is_standard_method(method: &str) -> bool {
    matches!(
        method,
        "initialize"
            | "ping"
            | "server/discover"
            | "prompts/get"
            | "prompts/list"
            | "resources/list"
            | "resources/read"
            | "resources/subscribe"
            | "resources/unsubscribe"
            | "resources/templates/list"
            | "subscriptions/listen"
            | "tools/call"
            | "tools/list"
    )
}

fn is_ignorable_notification(value: &Value) -> bool {
    const STANDARD: &[&str] = &[
        "notifications/cancelled",
        "notifications/initialized",
        "notifications/message",
        "notifications/progress",
        "notifications/prompts/list_changed",
        "notifications/resources/list_changed",
        "notifications/resources/updated",
        "notifications/roots/list_changed",
        "notifications/subscriptions/acknowledged",
        "notifications/tools/list_changed",
    ];
    if value.get("id").is_some() {
        return false;
    }
    let Some(method) = value.get("method").and_then(Value::as_str) else {
        return false;
    };
    !is_standard_method(method)
        || (method.starts_with("notifications/") && !STANDARD.contains(&method))
}

/// Classify the transport-independent envelope of one raw JSON-RPC
/// message — every check a malformed message fails before era-specific
/// policy applies. Both the stdio line gate and the streamable-HTTP body
/// gate run this so `mcp serve` and the daemon's `/mcp` endpoint share
/// one policy boundary.
pub fn classify_envelope(line: &[u8]) -> GateVerdict {
    // Parse errors are reported, not swallowed: a malformed message is
    // the one class where the peer provably needs the signal.
    let Ok(value) = serde_json::from_slice::<Value>(line) else {
        return reject(-32700, "Parse error", None, None);
    };
    // JSON-RPC batching is not defined by either pinned MCP revision; an
    // array on the wire is a malformed message, not a bundle.
    if value.is_array() {
        return reject(
            -32600,
            "Invalid request: JSON-RPC batches are not defined by MCP",
            None,
            None,
        );
    }
    let Some(object) = value.as_object() else {
        return reject(-32600, "Invalid request", None, None);
    };
    if object.get("jsonrpc").and_then(Value::as_str) != Some("2.0") {
        return reject(-32600, "Invalid request", None, None);
    }
    // `id: null` is not a notification — the spec forbids it outright.
    if object.get("id").is_some_and(Value::is_null) {
        return reject(
            -32600,
            "Invalid request: id must be a string or integer, never null",
            None,
            Some(Value::Null),
        );
    }
    let id = object.get("id").cloned();
    let method = object.get("method").and_then(Value::as_str);
    if id.is_some() && method.is_none() {
        return reject(-32600, "Invalid request", None, id);
    }
    if id.is_none() && method.is_none() {
        // No id and no method is not a message at all.
        return reject(-32600, "Invalid request", None, None);
    }
    // Compatibility: non-standard notifications are ignored, never
    // answered (rmcp's codec parity).
    if id.is_none() && is_ignorable_notification(&value) {
        return GateVerdict::Drop;
    }
    GateVerdict::Forward
}

/// The JSON-RPC error document both transports emit for a rejection —
/// the same envelope shape whether it travels as a stdio line or the
/// body of an HTTP 400.
fn error_json(code: i64, message: &str, data: Option<&Value>, id: Option<&Value>) -> Value {
    let mut error = serde_json::json!({"code": code, "message": message});
    if let Some(data) = data {
        error["data"] = data.clone();
    }
    serde_json::json!({
        "jsonrpc": "2.0",
        "id": id.cloned().unwrap_or(Value::Null),
        "error": error,
    })
}

/// HTTP entry point: `handle_request` classifies every buffered POST
/// body through the same envelope policy the stdio gate applies, so the
/// daemon/gateway surface and `mcp serve` share one boundary. Returns
/// the error document to answer with, or `None` when the body must reach
/// the service — forwards *and* compatibility drops both pass (the HTTP
/// layer answers notifications itself).
pub fn http_envelope_rejection(body: &[u8]) -> Option<Value> {
    match classify_envelope(body) {
        GateVerdict::Reject {
            code,
            message,
            data,
            id,
        } => Some(error_json(code, message, data.as_ref(), id.as_ref())),
        GateVerdict::Forward | GateVerdict::Drop => None,
    }
}

/// Classify one raw line on a stdio session. `classify_envelope` runs
/// first; a message that survives it is checked against the era-specific
/// `_meta` policy below. The verdict embeds everything the transport
/// needs to act — the caller owns emission, so the policy is also
/// testable in-process without a wire.
pub fn classify_line(line: &[u8], era: &mut EraState) -> GateVerdict {
    match classify_envelope(line) {
        GateVerdict::Forward => {}
        verdict @ (GateVerdict::Drop | GateVerdict::Reject { .. }) => return verdict,
    }
    // The envelope verdict proves this parse cannot fail.
    let value = match serde_json::from_slice::<Value>(line) {
        Ok(value) => value,
        Err(_) => return GateVerdict::Forward,
    };
    let Some(object) = value.as_object() else {
        return GateVerdict::Forward;
    };
    let id = object.get("id").cloned();
    let method = object.get("method").and_then(Value::as_str);
    let params = object.get("params").unwrap_or(&Value::Null);
    let meta = params.get("_meta").unwrap_or(&Value::Null);

    // Era tracking happens before the _meta policy: `initialize` pins the
    // negotiated revision, `server/discover` or any
    // `io.modelcontextprotocol/*` key marks the modern path.
    if method == Some("initialize") {
        era.note_initialize(
            params
                .get("protocolVersion")
                .and_then(Value::as_str)
                .unwrap_or_default(),
        );
    } else if method == Some("server/discover") {
        era.note_modern();
    }
    if let Some(version) = meta.get(META_PROTOCOL_VERSION).and_then(Value::as_str) {
        era.note_modern();
        if !MODERN_SUPPORTED_VERSIONS.contains(&version) {
            return reject(
                -32022,
                "Unsupported protocol version",
                Some(serde_json::json!({
                    "supported": MODERN_SUPPORTED_VERSIONS,
                    "requested": version,
                })),
                id,
            );
        }
    }

    // On the modern path every request MUST carry the per-request
    // protocol fields; a request missing any of them is malformed.
    // Notifications are exempt — they carry no result correlation.
    if id.is_some()
        && era.is_modern()
        && !matches!(method, Some("initialize") | Some("server/discover"))
        && REQUIRED_MODERN_META_KEYS
            .iter()
            .any(|key| meta.get(*key).is_none())
    {
        return reject(
            -32602,
            "Invalid params: missing required _meta protocol fields",
            None,
            id,
        );
    }

    GateVerdict::Forward
}

/// Era state carried across `classify_line` calls on one session.
#[derive(Debug, Default)]
pub struct EraState {
    era: Option<Era>,
}

impl EraState {
    /// A session is modern once it has shown any modern signal; it never
    /// falls back (an `initialize` after modern traffic is a peer error
    /// the service handles, not a downgrade).
    pub fn is_modern(&self) -> bool {
        self.era == Some(Era::Modern)
    }

    fn note_modern(&mut self) {
        self.era = Some(Era::Modern);
    }

    fn note_initialize(&mut self, version: &str) {
        if self.era.is_some() {
            return;
        }
        self.era = Some(if version == "2026-07-28" {
            Era::Modern
        } else {
            Era::Legacy
        });
    }
}

/// One shared writer for both service answers and gate rejections, so a
/// typed error the gate produces lands in the same byte stream rmcp
/// writes to — one newline-delimited JSON message per write call.
type SharedWriter<W> = Arc<Mutex<W>>;

/// The production stdio transport: a `Transport<RoleServer>` that runs
/// `classify_line` over every raw line before the typed codec sees it,
/// and stamps `resultType` onto outbound results that lack one.
pub struct StdioGate<R, W> {
    read: BufReader<R>,
    write: SharedWriter<W>,
    line_buf: Vec<u8>,
    era: EraState,
}

impl<R, W> StdioGate<R, W>
where
    R: AsyncRead + Unpin,
    W: AsyncWrite + Unpin + Send + 'static,
{
    pub fn new(read: R, write: W) -> Self {
        Self {
            read: BufReader::new(read),
            write: Arc::new(Mutex::new(write)),
            line_buf: Vec::new(),
            era: EraState::default(),
        }
    }

    /// Emit a JSON-RPC error response for a rejected line. `id` echoes
    /// the sender's id when the envelope carried a legible one.
    async fn emit_rejection(
        write: &SharedWriter<W>,
        code: i64,
        message: &str,
        data: Option<&Value>,
        id: Option<&Value>,
    ) -> std::io::Result<()> {
        let line = error_json(code, message, data, id);
        let mut bytes = serde_json::to_vec(&line)
            .map_err(|e| std::io::Error::new(std::io::ErrorKind::InvalidData, e.to_string()))?;
        bytes.push(b'\n');
        let mut guard = write.lock().await;
        guard.write_all(&bytes).await
    }

    /// Serialize a service answer with the era-correct `resultType`
    /// wire shape: on the modern path the field is required on results
    /// (stamp it when the payload lacks one); on a legacy session the
    /// field is not defined — the historical wire shape keeps it absent
    /// (the SDK already strips it; this is the belt-and-suspenders for
    /// custom results it cannot see).
    fn encode_outbound(
        item: &TxJsonRpcMessage<RoleServer>,
        modern: bool,
    ) -> std::io::Result<Vec<u8>> {
        let mut value = serde_json::to_value(item)
            .map_err(|e| std::io::Error::new(std::io::ErrorKind::InvalidData, e.to_string()))?;
        if let Some(result) = value.get_mut("result").and_then(Value::as_object_mut) {
            if modern {
                if !result.contains_key("resultType") {
                    result.insert(
                        "resultType".to_string(),
                        Value::String("complete".to_string()),
                    );
                }
            } else {
                result.remove("resultType");
            }
        }
        let mut bytes = serde_json::to_vec(&value)
            .map_err(|e| std::io::Error::new(std::io::ErrorKind::InvalidData, e.to_string()))?;
        bytes.push(b'\n');
        Ok(bytes)
    }
}

impl<R, W> Transport<RoleServer> for StdioGate<R, W>
where
    R: AsyncRead + Unpin + Send,
    W: AsyncWrite + Unpin + Send + 'static,
{
    type Error = std::io::Error;

    fn send(
        &mut self,
        item: TxJsonRpcMessage<RoleServer>,
    ) -> impl Future<Output = Result<(), Self::Error>> + Send + 'static {
        let write = Arc::clone(&self.write);
        let modern = self.era.is_modern();
        async move {
            let bytes = Self::encode_outbound(&item, modern)?;
            let mut guard = write.lock().await;
            guard.write_all(&bytes).await
        }
    }

    async fn receive(&mut self) -> Option<RxJsonRpcMessage<RoleServer>> {
        loop {
            // `receive` is polled inside a `select!` in the service
            // loop; `line_buf` lives on `self` so a cancelled read
            // resumes the same partial line instead of dropping it.
            match self.read.read_until(b'\n', &mut self.line_buf).await {
                Ok(0) => return None,
                Ok(_) => {}
                Err(_) => return None,
            }
            let mut line = std::mem::take(&mut self.line_buf);
            while line.last() == Some(&b'\n') || line.last() == Some(&b'\r') {
                line.pop();
            }
            if line.is_empty() {
                continue;
            }
            match classify_line(&line, &mut self.era) {
                GateVerdict::Forward => {
                    match serde_json::from_slice::<RxJsonRpcMessage<RoleServer>>(&line) {
                        Ok(message) => return Some(message),
                        Err(_) => {
                            if Self::emit_rejection(
                                &self.write,
                                -32600,
                                "Invalid request",
                                None,
                                None,
                            )
                            .await
                            .is_err()
                            {
                                return None;
                            }
                        }
                    }
                }
                GateVerdict::Drop => {}
                GateVerdict::Reject {
                    code,
                    message,
                    data,
                    id,
                } => {
                    if Self::emit_rejection(&self.write, code, message, data.as_ref(), id.as_ref())
                        .await
                        .is_err()
                    {
                        return None;
                    }
                }
            }
        }
    }

    async fn close(&mut self) -> Result<(), Self::Error> {
        Ok(())
    }
}

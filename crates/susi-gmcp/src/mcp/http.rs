//! MCP Streamable HTTP: one `/mcp` endpoint serving both lifecycles.
//!
//! - Legacy revisions (an `initialize` handshake) run a session per client:
//!   `initialize` creates it and answers with `Mcp-Session-Id`; each later
//!   request POSTs to it and is answered on its own SSE stream; GET opens
//!   the session's standalone stream; DELETE ends it.
//! - 2026-07-28 requests carry their metadata and are served statelessly,
//!   one request per POST, with the standard `Mcp-*` headers checked
//!   against the body.
//!
//! Host and Origin are validated before anything else (DNS-rebinding
//! defence); every SSE stream sends a keep-alive comment while idle.

use super::message::*;
use super::model::*;
use super::service::{serve_directly, serve_server, CancellationToken, DropGuard, ServerHandler};
use super::transport::Transport;
use bytes::Bytes;
use http_body_util::{combinators::BoxBody, BodyExt, Empty, Full};
use hyper::body::{Body, Frame};
use hyper::http::{self, HeaderMap, HeaderValue, Method, Request, Response, StatusCode};
use std::{
    collections::{HashMap, HashSet, VecDeque},
    convert::Infallible,
    future::Future,
    pin::Pin,
    sync::Arc,
    task::{Context, Poll},
    time::{Duration, Instant},
};
use tokio::sync::{mpsc, oneshot, RwLock};

pub type BoxResponse = Response<BoxBody<Bytes, Infallible>>;
type HttpResult<T> = Result<T, Box<BoxResponse>>;

pub const HEADER_SESSION_ID: &str = "Mcp-Session-Id";
pub const HEADER_LAST_EVENT_ID: &str = "Last-Event-Id";
pub const HEADER_MCP_PROTOCOL_VERSION: &str = "MCP-Protocol-Version";
pub const HEADER_MCP_METHOD: &str = "Mcp-Method";
pub const HEADER_MCP_NAME: &str = "Mcp-Name";
pub const HEADER_MCP_PARAM_PREFIX: &str = "Mcp-Param-";
pub const EVENT_STREAM_MIME_TYPE: &str = "text/event-stream";
pub const JSON_MIME_TYPE: &str = "application/json";
const BASE64_PREFIX: &str = "=?base64?";
const BASE64_SUFFIX: &str = "?=";
pub const DEFAULT_MAX_REQUEST_BODY_BYTES: usize = 4 * 1024 * 1024;

// ── Configuration ────────────────────────────────────────────────────────

#[derive(Debug, Clone)]
pub struct StreamableHttpServerConfig {
    /// Interval of the SSE keep-alive comment on an idle stream.
    pub sse_keep_alive: Option<Duration>,
    /// Reconnect delay advertised in each stream's priming event.
    pub sse_retry: Option<Duration>,
    pub max_request_body_bytes: usize,
    /// Accepted `Host` names (an entry without a port matches any port);
    /// empty accepts every host.
    pub allowed_hosts: Vec<String>,
    pub allowed_origins: Vec<String>,
    validate_empty_origin_allowlist: bool,
}

impl Default for StreamableHttpServerConfig {
    fn default() -> Self {
        Self {
            sse_keep_alive: Some(Duration::from_secs(15)),
            sse_retry: Some(Duration::from_secs(3)),
            max_request_body_bytes: DEFAULT_MAX_REQUEST_BODY_BYTES,
            allowed_hosts: vec!["localhost".into(), "127.0.0.1".into(), "::1".into()],
            allowed_origins: Vec::new(),
            validate_empty_origin_allowlist: false,
        }
    }
}

impl StreamableHttpServerConfig {
    pub fn with_allowed_hosts(
        mut self,
        hosts: impl IntoIterator<Item = impl Into<String>>,
    ) -> Self {
        self.allowed_hosts = hosts.into_iter().map(Into::into).collect();
        self
    }
    pub fn with_allowed_origins(
        mut self,
        origins: impl IntoIterator<Item = impl Into<String>>,
    ) -> Self {
        self.allowed_origins = origins.into_iter().map(Into::into).collect();
        self
    }
    /// Refuse every present `Origin` unless it is allow-listed, even when
    /// the allowlist is empty.
    pub fn enforce_origin_validation(mut self) -> Self {
        self.validate_empty_origin_allowlist = true;
        self
    }
}

#[derive(Debug, Clone)]
pub struct SessionConfig {
    pub channel_capacity: usize,
    /// A session with no event for this long is closed.
    pub keep_alive: Option<Duration>,
    pub sse_retry: Option<Duration>,
    /// How long a cancelled request's stream stays resumable.
    pub completed_cache_ttl: Duration,
    /// How long a new session waits for its `initialize`.
    pub init_timeout: Option<Duration>,
}

impl Default for SessionConfig {
    fn default() -> Self {
        Self {
            channel_capacity: 16,
            keep_alive: Some(Duration::from_secs(300)),
            sse_retry: Some(Duration::from_secs(3)),
            completed_cache_ttl: Duration::from_secs(60),
            init_timeout: Some(Duration::from_secs(60)),
        }
    }
}

// ── Responses ────────────────────────────────────────────────────────────

fn text_response(status: StatusCode, text: impl Into<Bytes>) -> BoxResponse {
    let mut response = Response::new(Full::new(text.into()).boxed());
    *response.status_mut() = status;
    response
}

fn boxed(response: BoxResponse) -> Box<BoxResponse> {
    Box::new(response)
}

fn accepted_response() -> BoxResponse {
    let mut response = Response::new(Empty::new().boxed());
    *response.status_mut() = StatusCode::ACCEPTED;
    response
}

fn method_not_allowed(allow: &'static str) -> BoxResponse {
    let mut response = text_response(StatusCode::METHOD_NOT_ALLOWED, "Method Not Allowed");
    response
        .headers_mut()
        .insert(http::header::ALLOW, HeaderValue::from_static(allow));
    response
}

fn session_id_required() -> BoxResponse {
    text_response(
        StatusCode::BAD_REQUEST,
        "Bad Request: Session ID is required",
    )
}

fn session_not_found() -> BoxResponse {
    text_response(StatusCode::NOT_FOUND, "Not Found: Session not found")
}

fn internal_error(context: &str, error: impl std::fmt::Display) -> BoxResponse {
    text_response(
        StatusCode::INTERNAL_SERVER_ERROR,
        format!("Encounter an error when {context}: {error}"),
    )
}

fn jsonrpc_response(message: &ServerJsonRpcMessage, status: StatusCode) -> BoxResponse {
    let body = serde_json::to_vec(message).unwrap_or_default();
    let mut response = text_response(status, body);
    response.headers_mut().insert(
        http::header::CONTENT_TYPE,
        HeaderValue::from_static(JSON_MIME_TYPE),
    );
    response
}

fn jsonrpc_error(id: Option<RequestId>, error: ErrorData) -> Box<BoxResponse> {
    boxed(jsonrpc_response(
        &ServerJsonRpcMessage::error(error, id),
        StatusCode::BAD_REQUEST,
    ))
}

fn invalid_request(id: Option<RequestId>, message: impl Into<String>) -> Box<BoxResponse> {
    jsonrpc_error(id, ErrorData::invalid_request(message.into(), None))
}

fn invalid_params(id: Option<RequestId>, message: impl Into<String>) -> Box<BoxResponse> {
    jsonrpc_error(id, ErrorData::invalid_params(message.into(), None))
}

fn header_mismatch(id: Option<RequestId>, message: impl Into<String>) -> Box<BoxResponse> {
    jsonrpc_error(id, ErrorData::header_mismatch(message.into(), None))
}

fn forbidden(message: &'static str) -> Box<BoxResponse> {
    boxed(text_response(StatusCode::FORBIDDEN, message))
}

fn bad_request_text(message: &'static str) -> Box<BoxResponse> {
    let mut response = text_response(StatusCode::BAD_REQUEST, message);
    response.headers_mut().insert(
        http::header::CONTENT_TYPE,
        HeaderValue::from_static("text/plain; charset=utf-8"),
    );
    boxed(response)
}

// ── Server-sent events ───────────────────────────────────────────────────

/// One SSE event: a message, a reconnect hint, or both.
#[derive(Debug, Clone, Default)]
pub struct SseEvent {
    pub event_id: Option<String>,
    pub message: Option<Arc<ServerJsonRpcMessage>>,
    pub retry: Option<Duration>,
}

impl SseEvent {
    fn message(message: ServerJsonRpcMessage) -> Self {
        Self {
            message: Some(Arc::new(message)),
            ..Self::default()
        }
    }
    fn priming(event_id: &str, retry: Duration) -> Self {
        Self {
            event_id: Some(event_id.to_string()),
            message: None,
            retry: Some(retry),
        }
    }
    fn retry(retry: Duration) -> Self {
        Self {
            retry: Some(retry),
            ..Self::default()
        }
    }
    fn to_bytes(&self) -> Bytes {
        let data = self
            .message
            .as_ref()
            .and_then(|m| serde_json::to_string(m.as_ref()).ok())
            .unwrap_or_default();
        let mut out = format!("data: {data}\n");
        if let Some(id) = &self.event_id {
            out.push_str(&format!("id: {id}\n"));
        }
        if let Some(retry) = self.retry {
            out.push_str(&format!("retry: {}\n", retry.as_millis()));
        }
        out.push('\n');
        Bytes::from(out)
    }
}

/// An SSE response body: queued events first, then a channel's events,
/// with a keep-alive comment whenever the stream idles. Dropping the body
/// (the client went away) fires its guard.
struct SseBody {
    queued: VecDeque<SseEvent>,
    events: Option<mpsc::Receiver<SseEvent>>,
    keep_alive: Option<(Duration, Pin<Box<tokio::time::Sleep>>)>,
    on_drop: Option<DropGuard>,
}

impl Body for SseBody {
    type Data = Bytes;
    type Error = Infallible;

    fn poll_frame(
        mut self: Pin<&mut Self>,
        cx: &mut Context<'_>,
    ) -> Poll<Option<Result<Frame<Bytes>, Infallible>>> {
        let this = &mut *self;
        let next = match this.queued.pop_front() {
            Some(event) => Poll::Ready(Some(event)),
            None => match this.events.as_mut() {
                Some(events) => events.poll_recv(cx),
                None => Poll::Ready(None),
            },
        };
        match next {
            Poll::Ready(Some(event)) => {
                if let Some((interval, sleep)) = this.keep_alive.as_mut() {
                    sleep
                        .as_mut()
                        .reset(tokio::time::Instant::now() + *interval);
                }
                Poll::Ready(Some(Ok(Frame::data(event.to_bytes()))))
            }
            Poll::Ready(None) => {
                // The stream ended normally: a completed request is not a
                // disconnect.
                if let Some(guard) = this.on_drop.take() {
                    guard.disarm();
                }
                Poll::Ready(None)
            }
            Poll::Pending => match this.keep_alive.as_mut() {
                Some((interval, sleep)) => match sleep.as_mut().poll(cx) {
                    Poll::Ready(()) => {
                        sleep
                            .as_mut()
                            .reset(tokio::time::Instant::now() + *interval);
                        Poll::Ready(Some(Ok(Frame::data(Bytes::from_static(b":\n\n")))))
                    }
                    Poll::Pending => Poll::Pending,
                },
                None => Poll::Pending,
            },
        }
    }
}

fn sse_response(
    queued: VecDeque<SseEvent>,
    events: Option<mpsc::Receiver<SseEvent>>,
    keep_alive: Option<Duration>,
    on_drop: Option<DropGuard>,
) -> BoxResponse {
    let body = SseBody {
        queued,
        events,
        keep_alive: keep_alive.map(|d| (d, Box::pin(tokio::time::sleep(d)))),
        on_drop,
    };
    let mut response = Response::new(body.boxed());
    let headers = response.headers_mut();
    headers.insert(
        http::header::CONTENT_TYPE,
        HeaderValue::from_static(EVENT_STREAM_MIME_TYPE),
    );
    headers.insert(
        http::header::CACHE_CONTROL,
        HeaderValue::from_static("no-cache"),
    );
    headers.insert("X-Accel-Buffering", HeaderValue::from_static("no"));
    response
}

// ── Header validation ────────────────────────────────────────────────────

fn header_text<'a>(headers: &'a HeaderMap, name: &str) -> Option<&'a str> {
    headers.get(name).and_then(|v| v.to_str().ok())
}

fn per_request_version(message: &ClientJsonRpcMessage) -> Option<ProtocolVersion> {
    if let ClientJsonRpcMessage::Request(r) = message {
        r.meta.protocol_version()
    } else {
        None
    }
}

fn validate_protocol_version_header(headers: &HeaderMap, allow_unknown: bool) -> HttpResult<()> {
    let Some(value) = headers.get(HEADER_MCP_PROTOCOL_VERSION) else {
        return Ok(());
    };
    let Ok(version) = value.to_str() else {
        return Err(boxed(text_response(
            StatusCode::BAD_REQUEST,
            "Bad Request: Invalid MCP-Protocol-Version header encoding",
        )));
    };
    let known = ProtocolVersion::KNOWN_VERSIONS
        .iter()
        .any(|v| v.as_str() == version);
    if !allow_unknown && !known {
        return Err(boxed(text_response(
            StatusCode::BAD_REQUEST,
            format!("Bad Request: Unsupported MCP-Protocol-Version: {version}"),
        )));
    }
    Ok(())
}

fn validate_header_matches_init_body(
    headers: &HeaderMap,
    body_version: &str,
    id: &RequestId,
) -> HttpResult<()> {
    let Some(value) = headers.get(HEADER_MCP_PROTOCOL_VERSION) else {
        return Ok(());
    };
    let Ok(header) = value.to_str() else {
        return Err(invalid_request(
            Some(id.clone()),
            "Invalid Request: MCP-Protocol-Version header is not valid UTF-8",
        ));
    };
    if header != body_version {
        return Err(invalid_request(
            Some(id.clone()),
            format!(
                "Invalid Request: MCP-Protocol-Version header ({header}) does not match initialize params.protocolVersion ({body_version})"
            ),
        ));
    }
    Ok(())
}

fn validate_request_protocol_version_meta(
    headers: &HeaderMap,
    message: &ClientJsonRpcMessage,
) -> HttpResult<()> {
    let ClientJsonRpcMessage::Request(request) = message else {
        return Ok(());
    };
    if matches!(request.request, ClientRequest::Initialize(_)) {
        return Ok(());
    }
    let header = header_text(headers, HEADER_MCP_PROTOCOL_VERSION);
    let Some(meta_version) = request.meta.protocol_version() else {
        let required = matches!(request.request, ClientRequest::Discover)
            || header.is_some_and(|v| v >= ProtocolVersion::V_2026_07_28.as_str());
        if required {
            let missing = request
                .meta
                .missing_required_keys(&ProtocolVersion::V_2026_07_28);
            return Err(invalid_params(
                Some(request.id.clone()),
                format!(
                    "Invalid params: request _meta is missing or has malformed required fields: {}",
                    missing.join(", ")
                ),
            ));
        }
        return Ok(());
    };
    let Some(header) = header else {
        return Err(header_mismatch(
            Some(request.id.clone()),
            "request _meta protocolVersion requires MCP-Protocol-Version header",
        ));
    };
    if header != meta_version.as_str() {
        return Err(header_mismatch(
            Some(request.id.clone()),
            format!(
                "MCP-Protocol-Version header ({header}) does not match request _meta protocolVersion ({meta_version})"
            ),
        ));
    }
    Ok(())
}

fn header_once<'a>(headers: &'a HeaderMap, name: &str) -> Result<Option<&'a str>, String> {
    let mut values = headers.get_all(name).iter();
    let Some(value) = values.next() else {
        return Ok(None);
    };
    if values.next().is_some() {
        return Err(format!("duplicate {name} header"));
    }
    Ok(value.to_str().ok())
}

fn base64_decode(text: &str) -> Option<Vec<u8>> {
    const ALPHABET: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
    let bytes = text.as_bytes();
    if !bytes.len().is_multiple_of(4) {
        return None;
    }
    let mut out = Vec::with_capacity(bytes.len() / 4 * 3);
    for (index, chunk) in bytes.chunks(4).enumerate() {
        let last = index + 1 == bytes.len() / 4;
        let pad = chunk.iter().rev().take_while(|b| **b == b'=').count();
        if pad > 2 || (pad > 0 && !last) {
            return None;
        }
        let mut acc = 0u32;
        for &b in &chunk[..4 - pad] {
            let v = ALPHABET.iter().position(|a| *a == b)? as u32;
            acc = (acc << 6) | v;
        }
        acc <<= 6 * pad as u32;
        let decoded = [(acc >> 16) as u8, (acc >> 8) as u8, acc as u8];
        out.extend_from_slice(&decoded[..3 - pad]);
    }
    Some(out)
}

fn decode_header_value(value: &str) -> Option<String> {
    match value
        .strip_prefix(BASE64_PREFIX)
        .and_then(|inner| inner.strip_suffix(BASE64_SUFFIX))
    {
        Some(inner) => String::from_utf8(base64_decode(inner)?).ok(),
        None => Some(value.to_string()),
    }
}

fn primitive_text(value: &serde_json::Value) -> Option<String> {
    match value {
        serde_json::Value::String(s) => Some(s.clone()),
        serde_json::Value::Bool(b) => Some(b.to_string()),
        serde_json::Value::Number(n) => Some(n.to_string()),
        serde_json::Value::Null | serde_json::Value::Array(_) | serde_json::Value::Object(_) => {
            None
        }
    }
}

/// `x-mcp-header` annotations on a tool schema's property chain.
fn param_header_annotations(schema: &JsonObject) -> Vec<(Vec<String>, String)> {
    fn walk(
        properties: Option<&serde_json::Value>,
        path: &mut Vec<String>,
        out: &mut Vec<(Vec<String>, String)>,
    ) {
        let Some(serde_json::Value::Object(properties)) = properties else {
            return;
        };
        for (name, schema) in properties {
            path.push(name.clone());
            if let Some(serde_json::Value::String(header)) = schema.get("x-mcp-header") {
                if !header.is_empty() {
                    out.push((path.clone(), header.clone()));
                }
            }
            walk(schema.get("properties"), path, out);
            path.pop();
        }
    }
    let mut out = Vec::new();
    walk(schema.get("properties"), &mut Vec::new(), &mut out);
    out
}

/// The 2026-07-28 standard headers must restate the body: `Mcp-Method`,
/// `Mcp-Name` for named methods, and each `x-mcp-header` tool argument.
fn validate_request_headers(
    headers: &HeaderMap,
    method: &str,
    params: Option<&serde_json::Value>,
    tool_schema: Option<&JsonObject>,
) -> Result<(), String> {
    match header_once(headers, HEADER_MCP_METHOD)? {
        None => return Err("missing required Mcp-Method header".to_string()),
        Some(value) if value != method => {
            return Err(format!(
                "Mcp-Method header `{value}` does not match body method `{method}`"
            ))
        }
        Some(_) => {}
    }
    let name_key = match method {
        "tools/call" | "prompts/get" => Some("name"),
        "resources/read" | "resources/subscribe" | "resources/unsubscribe" => Some("uri"),
        "tasks/get" | "tasks/update" | "tasks/cancel" => Some("taskId"),
        _ => None,
    };
    let expected = name_key
        .and_then(|key| params?.get(key)?.as_str())
        .map(str::to_string);
    if let Some(expected) = expected {
        match header_once(headers, HEADER_MCP_NAME)? {
            None => return Err(format!("missing required Mcp-Name header for `{method}`")),
            Some(raw) => {
                let decoded = decode_header_value(raw)
                    .ok_or_else(|| "Mcp-Name header is not valid Base64".to_string())?;
                if decoded != expected {
                    return Err(format!(
                        "Mcp-Name header `{decoded}` does not match body value `{expected}`"
                    ));
                }
            }
        }
    }
    if method == "tools/call" {
        if let Some(schema) = tool_schema {
            let arguments = params.and_then(|p| p.get("arguments"));
            for (path, header) in param_header_annotations(schema) {
                let prop = path.join(".");
                let full = format!("{HEADER_MCP_PARAM_PREFIX}{header}");
                let header_value = header_once(headers, &full)?;
                let body_value = arguments
                    .and_then(|a| path.iter().try_fold(a, |v, key| v.get(key)))
                    .filter(|v| !v.is_null())
                    .and_then(primitive_text);
                match (header_value, body_value) {
                    (None, None) => {}
                    (Some(_), None) => {
                        return Err(format!(
                            "unexpected {full} header for absent or null `{prop}`"
                        ))
                    }
                    (None, Some(_)) => return Err(format!("missing {full} header for `{prop}`")),
                    (Some(raw), Some(expected)) => {
                        let decoded = decode_header_value(raw)
                            .ok_or_else(|| format!("{full} header is not valid Base64"))?;
                        if decoded != expected {
                            return Err(format!(
                                "{full} header `{decoded}` does not match body value `{expected}`"
                            ));
                        }
                    }
                }
            }
        }
    }
    Ok(())
}

fn validate_standard_headers(
    headers: &HeaderMap,
    message: &ClientJsonRpcMessage,
    raw: &serde_json::Value,
    tool_schema: impl Fn(&str) -> Option<Arc<JsonObject>>,
) -> HttpResult<()> {
    let requires = header_text(headers, HEADER_MCP_PROTOCOL_VERSION)
        .is_some_and(|v| v >= ProtocolVersion::STANDARD_HEADERS.as_str());
    if !requires {
        return Ok(());
    }
    let id = match message {
        ClientJsonRpcMessage::Request(request) => {
            if matches!(request.request, ClientRequest::Initialize(_)) {
                let mut values = headers.get_all(HEADER_MCP_METHOD).iter();
                let Some(first) = values.next() else {
                    return Ok(());
                };
                if values.next().is_some() {
                    return Err(header_mismatch(
                        Some(request.id.clone()),
                        "multiple Mcp-Method header values supplied; initialize requires exactly one",
                    ));
                }
                if first.as_bytes() != b"initialize" {
                    return Err(header_mismatch(
                        Some(request.id.clone()),
                        format!(
                            "Mcp-Method header `{}` does not match body method `initialize`",
                            String::from_utf8_lossy(first.as_bytes())
                        ),
                    ));
                }
                return Ok(());
            }
            Some(request.id.clone())
        }
        ClientJsonRpcMessage::Notification(_) => None,
        ClientJsonRpcMessage::Response { .. } | ClientJsonRpcMessage::Error { .. } => return Ok(()),
    };
    let Some(method) = raw.get("method").and_then(serde_json::Value::as_str) else {
        return Ok(());
    };
    let params = raw.get("params");
    let schema = (method == "tools/call")
        .then(|| params?.get("name")?.as_str().and_then(&tool_schema))
        .flatten();
    validate_request_headers(headers, method, params, schema.as_deref())
        .map_err(|reason| header_mismatch(id, reason))
}

fn normalize_host(host: &str) -> String {
    host.trim_matches('[')
        .trim_matches(']')
        .to_ascii_lowercase()
}

fn parse_authority(text: &str) -> Option<(String, Option<u16>)> {
    let authority = http::uri::Authority::try_from(text).ok()?;
    Some((normalize_host(authority.host()), authority.port_u16()))
}

fn host_allowed(host: &(String, Option<u16>), allowed: &[String]) -> bool {
    if allowed.is_empty() {
        return true;
    }
    allowed.iter().any(|entry| {
        let entry = entry.trim();
        if entry.is_empty() {
            return false;
        }
        let (name, port) = parse_authority(entry).unwrap_or((normalize_host(entry), None));
        name == host.0 && port.is_none_or(|p| host.1 == Some(p))
    })
}

#[derive(Debug, PartialEq)]
enum Origin {
    Null,
    Tuple(String, String, Option<u16>),
}

fn parse_origin(value: &str) -> Option<Origin> {
    let value = value.trim();
    if value.is_empty() {
        return None;
    }
    if value.eq_ignore_ascii_case("null") {
        return Some(Origin::Null);
    }
    let uri = http::Uri::try_from(value).ok()?;
    let scheme = uri.scheme_str()?.to_ascii_lowercase();
    let authority = uri.authority()?;
    Some(Origin::Tuple(
        scheme,
        normalize_host(authority.host()),
        authority.port_u16(),
    ))
}

fn origin_allowed(origin: &Origin, allowed: &[String]) -> bool {
    allowed.iter().any(|raw| {
        let raw = raw.trim();
        let (base, any_port) = match raw.strip_suffix(":*") {
            Some(base) => (base, true),
            None => (raw, false),
        };
        let Some(entry) = parse_origin(base) else {
            return false;
        };
        match (&entry, origin) {
            (Origin::Null, Origin::Null) => !any_port,
            (Origin::Tuple(s, h, port), Origin::Tuple(os, oh, op)) => {
                if any_port && port.is_some() {
                    return false;
                }
                let default_port = match os.as_str() {
                    "http" => Some(80),
                    "https" => Some(443),
                    _ => None,
                };
                s == os && h == oh && (any_port || port.is_none() || op.or(default_port) == *port)
            }
            _ => false,
        }
    })
}

fn validate_dns_rebinding(
    uri: &http::Uri,
    headers: &HeaderMap,
    config: &StreamableHttpServerConfig,
) -> HttpResult<()> {
    let host = match headers.get(http::header::HOST) {
        Some(value) => {
            let text = value
                .to_str()
                .map_err(|_| bad_request_text("Bad Request: Invalid Host header encoding"))?;
            parse_authority(text)
                .ok_or_else(|| bad_request_text("Bad Request: Invalid Host header"))?
        }
        None => {
            let authority = uri
                .authority()
                .ok_or_else(|| bad_request_text("Bad Request: missing Host header"))?;
            (normalize_host(authority.host()), authority.port_u16())
        }
    };
    if !host_allowed(&host, &config.allowed_hosts) {
        return Err(forbidden("Forbidden: Host header is not allowed"));
    }
    if !config.validate_empty_origin_allowlist && config.allowed_origins.is_empty() {
        return Ok(());
    }
    let Some(origin) = headers.get(http::header::ORIGIN) else {
        return Ok(());
    };
    let text = origin
        .to_str()
        .map_err(|_| forbidden("Forbidden: Invalid Origin header encoding"))?;
    let origin = parse_origin(text).ok_or_else(|| forbidden("Forbidden: Invalid Origin header"))?;
    if !origin_allowed(&origin, &config.allowed_origins) {
        return Err(forbidden("Forbidden: Origin header is not allowed"));
    }
    Ok(())
}

/// Legacy (session) or 2026-07-28 (stateless) for this message and headers.
fn is_legacy_request(
    message: Option<&ClientJsonRpcMessage>,
    headers: &HeaderMap,
) -> HttpResult<bool> {
    let per_request = message.and_then(per_request_version).is_some();
    validate_protocol_version_header(headers, per_request)?;
    if let Some(message) = message {
        if let ClientJsonRpcMessage::Request(request) = message {
            if let ClientRequest::Initialize(init) = &request.request {
                validate_header_matches_init_body(
                    headers,
                    init.protocol_version.as_str(),
                    &request.id,
                )?;
            }
        }
        validate_request_protocol_version_meta(headers, message)?;
    }
    if let Some(ClientJsonRpcMessage::Request(request)) = message {
        if matches!(request.request, ClientRequest::Initialize(_)) {
            return Ok(true);
        }
    }
    let discover_lifecycle = matches!(message, Some(ClientJsonRpcMessage::Request(r))
        if r.meta.missing_required_keys(&ProtocolVersion::V_2026_07_28).is_empty());
    let version = message
        .and_then(per_request_version)
        .or_else(|| {
            header_text(headers, HEADER_MCP_PROTOCOL_VERSION)
                .and_then(|s| serde_json::from_value(serde_json::Value::String(s.to_string())).ok())
        })
        .unwrap_or(ProtocolVersion::V_2025_03_26);
    Ok(!discover_lifecycle && version.has_initialize())
}

fn jsonrpc_http_status(message: &ServerJsonRpcMessage, meta_malformed: bool) -> StatusCode {
    let ServerJsonRpcMessage::Error { error, .. } = message else {
        return StatusCode::OK;
    };
    match error.code {
        ErrorCode::UNSUPPORTED_PROTOCOL_VERSION
        | ErrorCode::MISSING_REQUIRED_CLIENT_CAPABILITY
        | ErrorCode::HEADER_MISMATCH => StatusCode::BAD_REQUEST,
        ErrorCode::INVALID_PARAMS if meta_malformed => StatusCode::BAD_REQUEST,
        ErrorCode::METHOD_NOT_FOUND => StatusCode::NOT_FOUND,
        _ => StatusCode::OK,
    }
}

// ── Stateless one-request transport ──────────────────────────────────────

/// Serves exactly one request: yields it, forwards every outbound message
/// to the HTTP response, and ends once the answer is sent.
struct OneshotTransport {
    message: Option<ClientJsonRpcMessage>,
    sender: mpsc::Sender<ServerJsonRpcMessage>,
    done: Arc<tokio::sync::Semaphore>,
}

impl Transport for OneshotTransport {
    fn send(
        &mut self,
        item: ServerJsonRpcMessage,
    ) -> impl Future<Output = std::io::Result<()>> + Send + 'static {
        let sender = self.sender.clone();
        let done = self.done.clone();
        let terminal = !matches!(item, ServerJsonRpcMessage::Notification(_));
        async move {
            sender.send(item).await.map_err(|_| {
                std::io::Error::new(std::io::ErrorKind::BrokenPipe, "response closed")
            })?;
            if terminal {
                done.add_permits(1);
            }
            Ok(())
        }
    }
    async fn receive(&mut self) -> Option<ClientJsonRpcMessage> {
        if let Some(message) = self.message.take() {
            return Some(message);
        }
        let _ = self.done.acquire().await;
        None
    }
    async fn close(&mut self) -> std::io::Result<()> {
        self.message.take();
        Ok(())
    }
}

// ── Sessions ─────────────────────────────────────────────────────────────

pub type SessionId = Arc<str>;
type HttpRequestId = u64;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct EventId {
    http_request_id: Option<HttpRequestId>,
    index: usize,
}

impl std::fmt::Display for EventId {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}", self.index)?;
        if let Some(id) = self.http_request_id {
            write!(f, "/{id}")?;
        }
        Ok(())
    }
}

impl std::str::FromStr for EventId {
    type Err = ();
    fn from_str(s: &str) -> Result<Self, ()> {
        match s.split_once('/') {
            Some((index, request)) => Ok(Self {
                http_request_id: Some(request.parse().map_err(|_| ())?),
                index: index.parse().map_err(|_| ())?,
            }),
            None => Ok(Self {
                http_request_id: None,
                index: s.parse().map_err(|_| ())?,
            }),
        }
    }
}

/// A stream sender that numbers its events and keeps the last
/// `capacity` of them for resumption.
struct CachedTx {
    tx: mpsc::Sender<SseEvent>,
    cache: VecDeque<SseEvent>,
    capacity: usize,
    http_request_id: Option<HttpRequestId>,
}

impl CachedTx {
    fn new(tx: mpsc::Sender<SseEvent>, http_request_id: Option<HttpRequestId>) -> Self {
        Self {
            capacity: tx.max_capacity(),
            cache: VecDeque::new(),
            tx,
            http_request_id,
        }
    }
    fn next_event_id(&self) -> EventId {
        let index = self
            .cache
            .back()
            .and_then(|e| e.event_id.as_deref()?.parse::<EventId>().ok())
            .map_or(0, |e| e.index + 1);
        EventId {
            http_request_id: self.http_request_id,
            index,
        }
    }
    async fn store_and_send(&mut self, mut event: SseEvent) {
        event.event_id = Some(self.next_event_id().to_string());
        if self.cache.len() >= self.capacity {
            self.cache.pop_front();
        }
        self.cache.push_back(event.clone());
        let _ = self.tx.send(event).await;
    }
    async fn send(&mut self, message: ServerJsonRpcMessage) {
        self.store_and_send(SseEvent::message(message)).await;
    }
    async fn send_priming(&mut self, retry: Duration) {
        self.store_and_send(SseEvent::retry(retry)).await;
    }
    /// Replay cached events from `index` on onto the current sender.
    async fn sync(&mut self, index: usize) -> Result<(), ()> {
        let Some(front) = self
            .cache
            .front()
            .and_then(|e| e.event_id.as_deref()?.parse::<EventId>().ok())
        else {
            return Ok(());
        };
        let skip = index.saturating_sub(front.index);
        if skip > self.cache.len() {
            return Err(());
        }
        for event in self.cache.iter().skip(skip) {
            if self.tx.send(event.clone()).await.is_err() {
                return Err(());
            }
        }
        Ok(())
    }
}

fn closed_sender() -> mpsc::Sender<SseEvent> {
    mpsc::channel(1).0
}

#[derive(Debug, Clone, Hash, PartialEq, Eq)]
enum ResourceKey {
    Request(RequestId),
    Progress(ProgressToken),
}

struct RequestStream {
    resources: HashSet<ResourceKey>,
    tx: CachedTx,
    completed_at: Option<Instant>,
}

enum SessionEvent {
    Message {
        message: ClientJsonRpcMessage,
        http_request_id: Option<HttpRequestId>,
    },
    OpenRequestStream {
        responder: oneshot::Sender<(HttpRequestId, mpsc::Receiver<SseEvent>)>,
    },
    OpenCommonStream {
        responder: oneshot::Sender<mpsc::Receiver<SseEvent>>,
    },
    Resume {
        last_event_id: EventId,
        responder: oneshot::Sender<Result<mpsc::Receiver<SseEvent>, ()>>,
    },
    Initialize {
        message: ClientJsonRpcMessage,
        responder: oneshot::Sender<ServerJsonRpcMessage>,
    },
    Close,
}

type Outbound = (ServerJsonRpcMessage, oneshot::Sender<std::io::Result<()>>);

/// The session's side of the service: messages from HTTP requests in,
/// every outbound message to the session worker for routing.
struct SessionTransport {
    inbound: mpsc::Receiver<ClientJsonRpcMessage>,
    outbound: mpsc::Sender<Outbound>,
}

impl Transport for SessionTransport {
    fn send(
        &mut self,
        item: ServerJsonRpcMessage,
    ) -> impl Future<Output = std::io::Result<()>> + Send + 'static {
        let outbound = self.outbound.clone();
        async move {
            let closed = || std::io::Error::new(std::io::ErrorKind::BrokenPipe, "session closed");
            let (tx, rx) = oneshot::channel();
            outbound.send((item, tx)).await.map_err(|_| closed())?;
            rx.await.map_err(|_| closed())?
        }
    }
    async fn receive(&mut self) -> Option<ClientJsonRpcMessage> {
        self.inbound.recv().await
    }
    async fn close(&mut self) -> std::io::Result<()> {
        Ok(())
    }
}

/// Routes one session's outbound messages to the HTTP streams they belong
/// to: an answer and its progress to the request's own stream (which ends
/// with the answer), everything else to the standalone stream.
struct SessionWorker {
    config: SessionConfig,
    next_http_request_id: HttpRequestId,
    streams: HashMap<HttpRequestId, RequestStream>,
    resources: HashMap<ResourceKey, HttpRequestId>,
    common: CachedTx,
    shadows: Vec<mpsc::Sender<SseEvent>>,
}

impl SessionWorker {
    fn unregister(&mut self, resource: &ResourceKey) {
        let Some(id) = self.resources.remove(resource) else {
            return;
        };
        let Some(stream) = self.streams.get_mut(&id) else {
            return;
        };
        if !stream.resources.is_empty() && !matches!(resource, ResourceKey::Request(_)) {
            return;
        }
        let resources: Vec<_> = stream.resources.drain().collect();
        stream.completed_at = Some(Instant::now());
        stream.tx.tx = closed_sender();
        for resource in resources {
            self.resources.remove(&resource);
        }
    }

    fn register(&mut self, resource: ResourceKey, id: HttpRequestId) {
        if let Some(stream) = self.streams.get_mut(&id) {
            stream.resources.insert(resource.clone());
            self.resources.insert(resource, id);
        }
    }

    async fn open_request_stream(&mut self) -> (HttpRequestId, mpsc::Receiver<SseEvent>) {
        let id = self.next_http_request_id;
        self.next_http_request_id = self.next_http_request_id.wrapping_add(1);
        let (tx, rx) = mpsc::channel(self.config.channel_capacity);
        let mut cached = CachedTx::new(tx, Some(id));
        if let Some(retry) = self.config.sse_retry {
            cached.send_priming(retry).await;
        }
        self.streams.insert(
            id,
            RequestStream {
                resources: HashSet::new(),
                tx: cached,
                completed_at: None,
            },
        );
        (id, rx)
    }

    async fn open_common(
        &mut self,
        last_event_index: Option<usize>,
    ) -> Result<mpsc::Receiver<SseEvent>, ()> {
        self.shadows.retain(|tx| !tx.is_closed());
        if self.common.tx.is_closed() {
            let (tx, rx) = mpsc::channel(self.config.channel_capacity);
            self.common.tx = tx;
            if let Some(index) = last_event_index {
                self.common.sync(index).await?;
            }
            Ok(rx)
        } else {
            // A second standalone stream only carries keep-alives.
            let (tx, rx) = mpsc::channel(1);
            if self.shadows.len() >= 32 {
                self.shadows.remove(0);
            }
            self.shadows.push(tx);
            Ok(rx)
        }
    }

    async fn resume(&mut self, last: EventId) -> Result<mpsc::Receiver<SseEvent>, ()> {
        match last.http_request_id {
            Some(id) => {
                let capacity = self.config.channel_capacity;
                let stream = self.streams.get_mut(&id).ok_or(())?;
                let completed = stream.completed_at.is_some();
                let (tx, rx) = mpsc::channel(capacity);
                stream.tx.tx = tx;
                stream.tx.sync(last.index).await?;
                if completed {
                    stream.tx.tx = closed_sender();
                }
                Ok(rx)
            }
            None => self.open_common(Some(last.index)).await,
        }
    }

    async fn route(&mut self, message: ServerJsonRpcMessage) -> std::io::Result<()> {
        let target = match &message {
            ServerJsonRpcMessage::Notification(n) => n
                .progress_token()
                .map(ResourceKey::Progress)
                .or_else(|| n.cancelled_request_id().map(ResourceKey::Request))
                .and_then(|key| self.resources.get(&key).copied())
                .map(|id| (id, false)),
            answer @ (ServerJsonRpcMessage::Response { .. }
            | ServerJsonRpcMessage::Error { .. }) => answer
                .answered_id()
                .and_then(|id| {
                    self.resources
                        .get(&ResourceKey::Request(id.clone()))
                        .copied()
                })
                .map(|id| (id, true)),
        };
        match target {
            Some((id, close)) => {
                let Some(stream) = self.streams.get_mut(&id) else {
                    return Err(std::io::Error::other(format!("Channel closed: {id}")));
                };
                stream.tx.send(message).await;
                if close {
                    if let Some(stream) = self.streams.remove(&id) {
                        for resource in stream.resources {
                            self.resources.remove(&resource);
                        }
                    }
                }
            }
            None => self.common.send(message).await,
        }
        Ok(())
    }

    async fn run(
        mut self,
        mut events: mpsc::Receiver<SessionEvent>,
        to_service: mpsc::Sender<ClientJsonRpcMessage>,
        mut from_service: mpsc::Receiver<Outbound>,
    ) {
        let init_timeout = self.config.init_timeout.unwrap_or(Duration::MAX);
        let first = tokio::select! {
            event = events.recv() => event,
            _ = tokio::time::sleep(init_timeout) => None,
        };
        let Some(SessionEvent::Initialize { message, responder }) = first else {
            return;
        };
        if to_service.send(message).await.is_err() {
            return;
        }
        let Some((answer, sent)) = from_service.recv().await else {
            return;
        };
        let _ = responder.send(answer);
        let _ = sent.send(Ok(()));
        let keep_alive = self.config.keep_alive.unwrap_or(Duration::MAX);
        loop {
            let ttl = self.config.completed_cache_ttl;
            self.streams
                .retain(|_, s| s.completed_at.is_none_or(|at| at.elapsed() < ttl));
            tokio::select! {
                event = events.recv() => match event {
                    None | Some(SessionEvent::Close) => return,
                    Some(SessionEvent::Message { message, http_request_id }) => {
                        match &message {
                            ClientJsonRpcMessage::Request(request) => {
                                if let Some(id) = http_request_id {
                                    self.register(ResourceKey::Request(request.id.clone()), id);
                                    if let Some(token) = request.meta.get_progress_token() {
                                        self.register(ResourceKey::Progress(token), id);
                                    }
                                }
                            }
                            ClientJsonRpcMessage::Notification(n) => {
                                if let ClientNotificationKind::Cancelled(c) = &n.kind {
                                    if let Some(id) = c.request_id.clone() {
                                        self.unregister(&ResourceKey::Request(id));
                                    }
                                }
                            }
                            ClientJsonRpcMessage::Response { .. } | ClientJsonRpcMessage::Error { .. } => {}
                        }
                        if to_service.send(message).await.is_err() {
                            return;
                        }
                    }
                    Some(SessionEvent::OpenRequestStream { responder }) => {
                        let _ = responder.send(self.open_request_stream().await);
                    }
                    Some(SessionEvent::OpenCommonStream { responder }) => {
                        if let Ok(rx) = self.open_common(Some(0)).await {
                            let _ = responder.send(rx);
                        }
                    }
                    Some(SessionEvent::Resume { last_event_id, responder }) => {
                        let _ = responder.send(self.resume(last_event_id).await);
                    }
                    Some(SessionEvent::Initialize { .. }) => {}
                },
                outbound = from_service.recv() => {
                    let Some((message, sent)) = outbound else { return };
                    let answered = message.answered_id().cloned();
                    let _ = sent.send(self.route(message).await);
                    if let Some(id) = answered {
                        self.unregister(&ResourceKey::Request(id));
                    }
                }
                _ = tokio::time::sleep(keep_alive) => return,
            }
        }
    }
}

#[derive(Clone)]
struct SessionHandle {
    events: mpsc::Sender<SessionEvent>,
}

impl SessionHandle {
    async fn call<T>(
        &self,
        make: impl FnOnce(oneshot::Sender<T>) -> SessionEvent,
    ) -> std::io::Result<T> {
        let terminated = || std::io::Error::other("Session error: Session service terminated");
        let (tx, rx) = oneshot::channel();
        self.events.send(make(tx)).await.map_err(|_| terminated())?;
        rx.await.map_err(|_| terminated())
    }
    async fn push(
        &self,
        message: ClientJsonRpcMessage,
        http_request_id: Option<HttpRequestId>,
    ) -> std::io::Result<()> {
        self.events
            .send(SessionEvent::Message {
                message,
                http_request_id,
            })
            .await
            .map_err(|_| std::io::Error::other("Session error: Session service terminated"))
    }
}

/// The live sessions of one HTTP surface.
#[derive(Default)]
pub struct LocalSessionManager {
    sessions: RwLock<HashMap<SessionId, SessionHandle>>,
    pub session_config: SessionConfig,
}

impl LocalSessionManager {
    async fn handle(&self, id: &SessionId) -> Option<SessionHandle> {
        self.sessions.read().await.get(id).cloned()
    }
    async fn close_session(&self, id: &SessionId) {
        let handle = self.sessions.write().await.remove(id);
        if let Some(handle) = handle {
            let _ = handle.events.send(SessionEvent::Close).await;
        }
    }
    fn create_session(
        &self,
    ) -> (
        SessionId,
        SessionHandle,
        SessionTransport,
        SessionWorkerParts,
    ) {
        let id: SessionId = super::task_manager::uuid_v4().into();
        let capacity = self.session_config.channel_capacity;
        let (events_tx, events_rx) = mpsc::channel(capacity);
        let (to_service, inbound) = mpsc::channel(capacity);
        let (outbound, from_service) = mpsc::channel(capacity);
        let worker = SessionWorker {
            config: self.session_config.clone(),
            next_http_request_id: 0,
            streams: HashMap::new(),
            resources: HashMap::new(),
            common: CachedTx::new(closed_sender(), None),
            shadows: Vec::new(),
        };
        (
            id,
            SessionHandle { events: events_tx },
            SessionTransport { inbound, outbound },
            SessionWorkerParts {
                worker,
                events: events_rx,
                to_service,
                from_service,
            },
        )
    }
}

struct SessionWorkerParts {
    worker: SessionWorker,
    events: mpsc::Receiver<SessionEvent>,
    to_service: mpsc::Sender<ClientJsonRpcMessage>,
    from_service: mpsc::Receiver<Outbound>,
}

// ── The service ──────────────────────────────────────────────────────────

/// The `/mcp` endpoint for a handler `S`, with sessions kept by `M`.
pub struct StreamableHttpService<S, M = LocalSessionManager> {
    pub config: StreamableHttpServerConfig,
    session_manager: Arc<M>,
    service_factory: Arc<dyn Fn() -> Result<S, std::io::Error> + Send + Sync>,
}

impl<S, M> Clone for StreamableHttpService<S, M> {
    fn clone(&self) -> Self {
        Self {
            config: self.config.clone(),
            session_manager: self.session_manager.clone(),
            service_factory: self.service_factory.clone(),
        }
    }
}

impl<S: ServerHandler> StreamableHttpService<S, LocalSessionManager> {
    pub fn new(
        service_factory: impl Fn() -> Result<S, std::io::Error> + Send + Sync + 'static,
        session_manager: Arc<LocalSessionManager>,
        config: StreamableHttpServerConfig,
    ) -> Self {
        Self {
            config,
            session_manager,
            service_factory: Arc::new(service_factory),
        }
    }

    fn service(&self) -> Result<S, Box<BoxResponse>> {
        (self.service_factory)().map_err(|e| boxed(internal_error("get service", e)))
    }

    /// Answer one HTTP request.
    pub async fn handle<B>(&self, request: Request<B>) -> BoxResponse
    where
        B: Body + Send + 'static,
        B::Error: std::fmt::Display,
    {
        if let Err(response) =
            validate_dns_rebinding(request.uri(), request.headers(), &self.config)
        {
            return *response;
        }
        let result = match *request.method() {
            Method::POST => self.handle_post(request).await,
            Method::GET => self.handle_get(request).await,
            Method::DELETE => self.handle_delete(request).await,
            _ => return method_not_allowed("GET, POST, DELETE"),
        };
        result.unwrap_or_else(|response| *response)
    }

    async fn handle_get<B>(&self, request: Request<B>) -> HttpResult<BoxResponse> {
        if !header_text(request.headers(), "accept")
            .is_some_and(|a| a.contains(EVENT_STREAM_MIME_TYPE))
        {
            return Ok(text_response(
                StatusCode::NOT_ACCEPTABLE,
                "Not Acceptable: Client must accept text/event-stream",
            ));
        }
        if !is_legacy_request(None, request.headers())? {
            // No event store: a stateless stream cannot be resumed.
            return Ok(method_not_allowed("POST"));
        }
        let Some(session_id) =
            header_text(request.headers(), HEADER_SESSION_ID).map(SessionId::from)
        else {
            return Ok(session_id_required());
        };
        let Some(session) = self.session_manager.handle(&session_id).await else {
            return Ok(session_not_found());
        };
        validate_protocol_version_header(request.headers(), false)?;
        let keep_alive = self.config.sse_keep_alive;
        if let Some(last) = header_text(request.headers(), HEADER_LAST_EVENT_ID) {
            let resumed = match last.parse::<EventId>() {
                Ok(last_event_id) => session
                    .call(|responder| SessionEvent::Resume {
                        last_event_id,
                        responder,
                    })
                    .await
                    .ok()
                    .and_then(Result::ok),
                Err(()) => None,
            };
            return Ok(match resumed {
                Some(rx) => sse_response(VecDeque::new(), Some(rx), keep_alive, None),
                None => sse_response(VecDeque::new(), None, None, None),
            });
        }
        let rx = session
            .call(|responder| SessionEvent::OpenCommonStream { responder })
            .await
            .map_err(|e| boxed(internal_error("create standalone stream", e)))?;
        let mut queued = VecDeque::new();
        if let Some(retry) = self.config.sse_retry {
            queued.push_back(SseEvent::priming("0", retry));
        }
        Ok(sse_response(queued, Some(rx), keep_alive, None))
    }

    async fn handle_delete<B>(&self, request: Request<B>) -> HttpResult<BoxResponse> {
        if !is_legacy_request(None, request.headers())? {
            return Ok(method_not_allowed("POST"));
        }
        let Some(session_id) =
            header_text(request.headers(), HEADER_SESSION_ID).map(SessionId::from)
        else {
            return Ok(session_id_required());
        };
        validate_protocol_version_header(request.headers(), false)?;
        self.session_manager.close_session(&session_id).await;
        Ok(accepted_response())
    }

    async fn read_body<B>(&self, body: B) -> HttpResult<Vec<u8>>
    where
        B: Body + Send + 'static,
        B::Error: std::fmt::Display,
    {
        let max = self.config.max_request_body_bytes;
        let mut collected = Vec::new();
        let mut body = std::pin::pin!(body);
        while let Some(frame) = std::future::poll_fn(|cx| body.as_mut().poll_frame(cx)).await {
            let frame = frame.map_err(|e| {
                boxed(text_response(
                    StatusCode::INTERNAL_SERVER_ERROR,
                    format!("Failed to read request body: {e}"),
                ))
            })?;
            let Ok(mut data) = frame.into_data() else {
                continue;
            };
            use bytes::Buf;
            if data.remaining() > max.saturating_sub(collected.len()) {
                return Err(boxed(text_response(
                    StatusCode::PAYLOAD_TOO_LARGE,
                    format!("Payload Too Large: request body exceeds {max} bytes"),
                )));
            }
            while data.has_remaining() {
                let chunk = data.chunk();
                collected.extend_from_slice(chunk);
                let n = chunk.len();
                data.advance(n);
            }
        }
        Ok(collected)
    }

    async fn handle_post<B>(&self, request: Request<B>) -> HttpResult<BoxResponse>
    where
        B: Body + Send + 'static,
        B::Error: std::fmt::Display,
    {
        let headers = request.headers();
        if !header_text(headers, "accept")
            .is_some_and(|a| a.contains(JSON_MIME_TYPE) && a.contains(EVENT_STREAM_MIME_TYPE))
        {
            return Ok(text_response(
                StatusCode::NOT_ACCEPTABLE,
                "Not Acceptable: Client must accept both application/json and text/event-stream",
            ));
        }
        if !header_text(headers, "content-type").is_some_and(|c| c.starts_with(JSON_MIME_TYPE)) {
            return Ok(text_response(
                StatusCode::UNSUPPORTED_MEDIA_TYPE,
                "Unsupported Media Type: Content-Type must be application/json",
            ));
        }
        let (parts, body) = request.into_parts();
        let bytes = self.read_body(body).await?;
        let raw: serde_json::Value = match serde_json::from_slice(&bytes) {
            Ok(raw) => raw,
            Err(e) => {
                return Ok(text_response(
                    StatusCode::UNSUPPORTED_MEDIA_TYPE,
                    format!("fail to deserialize request body {e}"),
                ))
            }
        };
        let Some(message) = ClientJsonRpcMessage::from_value(raw.clone()) else {
            return Ok(text_response(
                StatusCode::UNSUPPORTED_MEDIA_TYPE,
                "fail to deserialize request body unrecognized JSON-RPC message",
            ));
        };
        let headers = &parts.headers;
        if is_legacy_request(Some(&message), headers)? {
            return self.handle_session_post(message, raw, headers).await;
        }
        let per_request = per_request_version(&message).is_some();
        match &message {
            ClientJsonRpcMessage::Request(request) => {
                if let ClientRequest::Initialize(init) = &request.request {
                    validate_header_matches_init_body(
                        headers,
                        init.protocol_version.as_str(),
                        &request.id,
                    )?;
                } else {
                    validate_protocol_version_header(headers, per_request)?;
                }
            }
            ClientJsonRpcMessage::Notification(_)
            | ClientJsonRpcMessage::Response { .. }
            | ClientJsonRpcMessage::Error { .. } => {
                validate_protocol_version_header(headers, per_request)?
            }
        }
        let service = self.service()?;
        validate_standard_headers(headers, &message, &raw, |name| {
            service.get_tool(name).map(|tool| tool.input_schema)
        })?;
        validate_request_protocol_version_meta(headers, &message)?;
        match message {
            ClientJsonRpcMessage::Request(request) => {
                Ok(self.serve_stateless(service, request, headers).await)
            }
            ClientJsonRpcMessage::Notification(_) => Ok(accepted_response()),
            ClientJsonRpcMessage::Response { .. } | ClientJsonRpcMessage::Error { .. } => Err(
                invalid_request(None, "stateless server does not accept JSON-RPC responses"),
            ),
        }
    }

    async fn handle_session_post(
        &self,
        message: ClientJsonRpcMessage,
        raw: serde_json::Value,
        headers: &HeaderMap,
    ) -> HttpResult<BoxResponse> {
        let tool_schema = |name: &str| {
            (self.service_factory)()
                .ok()?
                .get_tool(name)
                .map(|tool| tool.input_schema)
        };
        if let Some(session_id) = header_text(headers, HEADER_SESSION_ID).map(SessionId::from) {
            let Some(session) = self.session_manager.handle(&session_id).await else {
                return Ok(session_not_found());
            };
            validate_protocol_version_header(headers, per_request_version(&message).is_some())?;
            validate_request_protocol_version_meta(headers, &message)?;
            validate_standard_headers(headers, &message, &raw, tool_schema)?;
            return match message {
                ClientJsonRpcMessage::Request(_) => {
                    let (id, rx) = session
                        .call(|responder| SessionEvent::OpenRequestStream { responder })
                        .await
                        .map_err(|e| boxed(internal_error("get session", e)))?;
                    session
                        .push(message, Some(id))
                        .await
                        .map_err(|e| boxed(internal_error("get session", e)))?;
                    Ok(sse_response(
                        VecDeque::new(),
                        Some(rx),
                        self.config.sse_keep_alive,
                        None,
                    ))
                }
                ClientJsonRpcMessage::Notification(_)
                | ClientJsonRpcMessage::Response { .. }
                | ClientJsonRpcMessage::Error { .. } => {
                    session
                        .push(message, None)
                        .await
                        .map_err(|e| boxed(internal_error("accept message", e)))?;
                    Ok(accepted_response())
                }
            };
        }
        if let ClientJsonRpcMessage::Request(request) = &message {
            if matches!(request.request, ClientRequest::Discover) {
                validate_protocol_version_header(headers, per_request_version(&message).is_some())?;
                validate_standard_headers(headers, &message, &raw, tool_schema)?;
                validate_request_protocol_version_meta(headers, &message)?;
                let ClientJsonRpcMessage::Request(request) = message else {
                    return Ok(session_id_required());
                };
                let service = self.service()?;
                return Ok(self.serve_stateless(service, request, headers).await);
            }
        }
        validate_standard_headers(headers, &message, &raw, tool_schema)?;
        let ClientJsonRpcMessage::Request(request) = &message else {
            return Ok(session_id_required());
        };
        let ClientRequest::Initialize(init) = &request.request else {
            return Ok(session_id_required());
        };
        validate_header_matches_init_body(headers, init.protocol_version.as_str(), &request.id)?;
        let service = self.service()?;
        let (session_id, handle, transport, parts) = self.session_manager.create_session();
        self.session_manager
            .sessions
            .write()
            .await
            .insert(session_id.clone(), handle.clone());
        let SessionWorkerParts {
            worker,
            events,
            to_service,
            from_service,
        } = parts;
        tokio::spawn(worker.run(events, to_service, from_service));
        let manager = self.session_manager.clone();
        let closing = session_id.clone();
        tokio::spawn(async move {
            if let Ok(running) = serve_server(service, transport, CancellationToken::new()).await {
                let _ = running.waiting().await;
            }
            manager.close_session(&closing).await;
        });
        let answer = handle
            .call(|responder| SessionEvent::Initialize { message, responder })
            .await
            .map_err(|e| boxed(internal_error("create stream", e)))?;
        let mut queued = VecDeque::new();
        if let Some(retry) = self.config.sse_retry {
            queued.push_back(SseEvent::priming("0", retry));
        }
        queued.push_back(SseEvent::message(answer));
        let mut response = sse_response(queued, None, self.config.sse_keep_alive, None);
        let value = HeaderValue::from_str(&session_id)
            .map_err(|e| boxed(internal_error("create session id header", e)))?;
        response.headers_mut().insert(HEADER_SESSION_ID, value);
        Ok(response)
    }

    /// Serve one self-describing (2026-07-28) request outside any session.
    async fn serve_stateless(
        &self,
        service: S,
        request: JsonRpcRequest,
        headers: &HeaderMap,
    ) -> BoxResponse {
        let meta_malformed = !request
            .meta
            .missing_required_keys(&ProtocolVersion::V_2026_07_28)
            .is_empty();
        let version = header_text(headers, HEADER_MCP_PROTOCOL_VERSION)
            .and_then(|s| serde_json::from_value(serde_json::Value::String(s.to_string())).ok())
            .unwrap_or(ProtocolVersion::V_2025_03_26);
        let peer_info = InitializeRequestParams {
            protocol_version: version,
            ..InitializeRequestParams::default()
        };
        let (sender, mut receiver) = mpsc::channel(16);
        let transport = OneshotTransport {
            message: Some(ClientJsonRpcMessage::Request(request)),
            sender,
            done: Arc::new(tokio::sync::Semaphore::new(0)),
        };
        let request_ct = CancellationToken::new();
        let running = serve_directly(service, transport, Some(peer_info), request_ct.clone());
        tokio::spawn(async move {
            let _ = running.waiting().await;
        });
        // A client that hangs up before the first answer cancels the request.
        let disconnect = request_ct.clone().drop_guard();
        let first = receiver.recv().await;
        let disconnect = disconnect.disarm();
        let Some(first) = first else {
            return internal_error(
                "empty response",
                "no response message received from handler",
            );
        };
        let terminal = !matches!(first, ServerJsonRpcMessage::Notification(_));
        let status = jsonrpc_http_status(&first, meta_malformed);
        if terminal && status != StatusCode::OK {
            disconnect.cancel();
            return jsonrpc_response(&first, status);
        }
        let mut queued = VecDeque::new();
        queued.push_back(SseEvent::message(first));
        let (tx, rx) = mpsc::channel(16);
        tokio::spawn(async move {
            while let Some(message) = receiver.recv().await {
                if tx.send(SseEvent::message(message)).await.is_err() {
                    break;
                }
            }
        });
        sse_response(
            queued,
            Some(rx),
            self.config.sse_keep_alive,
            Some(disconnect.drop_guard()),
        )
    }
}

impl<B, S> tower_service::Service<Request<B>> for StreamableHttpService<S, LocalSessionManager>
where
    B: Body + Send + 'static,
    B::Error: std::fmt::Display,
    S: ServerHandler,
{
    type Response = BoxResponse;
    type Error = Infallible;
    type Future = Pin<Box<dyn Future<Output = Result<BoxResponse, Infallible>> + Send>>;

    fn poll_ready(&mut self, _cx: &mut Context<'_>) -> Poll<Result<(), Infallible>> {
        Poll::Ready(Ok(()))
    }

    fn call(&mut self, request: Request<B>) -> Self::Future {
        let service = self.clone();
        Box::pin(async move { Ok(service.handle(request).await) })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn base64_header_values_decode() {
        assert_eq!(decode_header_value("plain").as_deref(), Some("plain"));
        assert_eq!(
            decode_header_value("=?base64?Y2Fmw6k=?=").as_deref(),
            Some("café")
        );
        assert_eq!(decode_header_value("=?base64?***?="), None);
    }

    #[test]
    fn event_ids_round_trip() {
        for text in ["0", "3/7"] {
            assert_eq!(text.parse::<EventId>().unwrap().to_string(), text);
        }
        assert!("x/1".parse::<EventId>().is_err());
    }

    #[test]
    fn hosts_and_origins_follow_the_allowlists() {
        let allowed = vec!["localhost".to_string(), "127.0.0.1:9090".to_string()];
        assert!(host_allowed(&("localhost".into(), Some(1)), &allowed));
        assert!(host_allowed(&("127.0.0.1".into(), Some(9090)), &allowed));
        assert!(!host_allowed(&("127.0.0.1".into(), Some(1)), &allowed));
        let origins = vec![
            "https://app.example".to_string(),
            "http://[::1]:*".to_string(),
        ];
        assert!(origin_allowed(
            &parse_origin("https://app.example:8443").unwrap(),
            &origins
        ));
        assert!(origin_allowed(
            &parse_origin("http://[::1]:3000").unwrap(),
            &origins
        ));
        assert!(!origin_allowed(
            &parse_origin("https://evil.example").unwrap(),
            &origins
        ));
    }

    #[test]
    fn sse_events_serialize_like_the_wire() {
        let priming = SseEvent::priming("0", Duration::from_secs(3));
        assert_eq!(&priming.to_bytes()[..], b"data: \nid: 0\nretry: 3000\n\n");
    }
}

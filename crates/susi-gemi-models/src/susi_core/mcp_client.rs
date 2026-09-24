//! Blocking session-aware MCP `tools/call` client for the Streamable HTTP
//! transport served by `susi-gmcp` (rmcp SDK `StreamableHttpService`).
//!
//! The peer channel contract, verified against a live daemon:
//!
//! ```text
//! POST http://<addr>/mcp  initialize
//!     Accept: application/json, text/event-stream
//!     -> `Mcp-Session-Id` response header
//! POST /mcp  notifications/initialized   (session + protocol headers) -> 202
//! POST /mcp  tools/call                  (session + protocol headers)
//!     -> SSE body: `data: {"jsonrpc":"2.0","id":N,"result":{...}}`
//! ```
//!
//! Posting `tools/call` directly to `/` or without the session handshake is
//! rejected by the server (404 / "expect initialize request") — the naive
//! single-shot POST this module replaced silently 404'd, which is how peer
//! dispatch and commit replication went dead in production while still
//! "succeeding" from the caller's perspective.

use serde_json::{json, Value};

/// Protocol revision the server negotiates; matched against
/// `crates/susi-gmcp/src/protocol_tests.rs`.
const PROTOCOL_VERSION: &str = "2025-11-25";

fn post(
    url: &str,
    bearer: Option<&str>,
    session: Option<&str>,
    body: &Value,
) -> Result<ureq::http::Response<ureq::Body>, String> {
    let mut req = crate::susi_config::http_agent()
        .post(url)
        .header("accept", "application/json, text/event-stream");
    if let Some(token) = bearer {
        req = req.header("authorization", format!("Bearer {token}"));
    }
    if let Some(sid) = session {
        req = req
            .header("mcp-session-id", sid)
            .header("mcp-protocol-version", PROTOCOL_VERSION);
    }
    req.send_json(body).map_err(|e| format!("POST {url}: {e}"))
}

/// Extract the `data:` JSON-RPC message matching `id` from an SSE body.
/// Control events (`data:` followed by `id:`/`retry:` lines) and unrelated
/// notifications are skipped.
fn sse_result(body: &str, id: u64) -> Result<Value, String> {
    for line in body.lines() {
        let Some(data) = line.strip_prefix("data:") else {
            continue;
        };
        let Ok(msg) = serde_json::from_str::<Value>(data.trim()) else {
            continue;
        };
        if msg.get("id").and_then(Value::as_u64) != Some(id) {
            continue;
        }
        if let Some(err) = msg.get("error") {
            return Err(format!("peer rpc error: {err}"));
        }
        return msg
            .get("result")
            .cloned()
            .ok_or_else(|| "response carries neither result nor error".to_string());
    }
    Err("no matching response frame in SSE stream".to_string())
}

/// Invoke `tool` on the peer at `addr` (`host:port`), returning the raw
/// `result` object of the `tools/call` response — callers extract the tool's
/// text payload via `result.pointer("/content/0/text")`.
///
/// `bearer` is the host api_token when the caller is entitled to present it
/// (the admission decision belongs to the caller — this client forwards
/// whatever it is given).
pub fn call_tool(
    addr: &str,
    tool: &str,
    arguments: &Value,
    bearer: Option<&str>,
) -> Result<Value, String> {
    let url = format!("http://{addr}/mcp");

    // 1. initialize — the response header carries the session id.
    let init = post(
        &url,
        bearer,
        None,
        &json!({
            "jsonrpc": "2.0",
            "id": 1,
            "method": "initialize",
            "params": {
                "protocolVersion": PROTOCOL_VERSION,
                "capabilities": {},
                "clientInfo": {"name": "susi", "version": env!("CARGO_PKG_VERSION")}
            }
        }),
    )?;
    let session = init
        .headers()
        .get("mcp-session-id")
        .and_then(|v| v.to_str().ok())
        .map(str::to_string)
        .ok_or_else(|| format!("peer {addr} returned no Mcp-Session-Id"))?;

    // 2. notifications/initialized — required before the session accepts
    //    further messages.
    post(
        &url,
        bearer,
        Some(&session),
        &json!({"jsonrpc": "2.0", "method": "notifications/initialized"}),
    )?;

    // 3. tools/call — the SSE body carries the response frame.
    session_request(
        &url,
        bearer,
        &session,
        "tools/call",
        &json!({
            "name": tool,
            "arguments": arguments
        }),
    )
}

/// Run any session-scoped JSON-RPC method (`tools/list`, `resources/list`,
/// `ping`, ...) through the same initialize → initialized → request
/// handshake `call_tool` uses. Returns the frame's `result` object.
pub fn session_call(
    addr: &str,
    method: &str,
    params: &Value,
    bearer: Option<&str>,
) -> Result<Value, String> {
    let url = format!("http://{addr}/mcp");
    let init = post(
        &url,
        bearer,
        None,
        &json!({
            "jsonrpc": "2.0",
            "id": 1,
            "method": "initialize",
            "params": {
                "protocolVersion": PROTOCOL_VERSION,
                "capabilities": {},
                "clientInfo": {"name": "susi", "version": env!("CARGO_PKG_VERSION")}
            }
        }),
    )?;
    let session = init
        .headers()
        .get("mcp-session-id")
        .and_then(|v| v.to_str().ok())
        .map(str::to_string)
        .ok_or_else(|| format!("peer {addr} returned no Mcp-Session-Id"))?;
    post(
        &url,
        bearer,
        Some(&session),
        &json!({"jsonrpc": "2.0", "method": "notifications/initialized"}),
    )?;
    session_request(&url, bearer, &session, method, params)
}

/// POST one request inside an established session and parse the matching
/// SSE response frame.
fn session_request(
    url: &str,
    bearer: Option<&str>,
    session: &str,
    method: &str,
    params: &Value,
) -> Result<Value, String> {
    let resp = post(
        url,
        bearer,
        Some(session),
        &json!({
            "jsonrpc": "2.0",
            "id": 2,
            "method": method,
            "params": params
        }),
    )?;
    let text = resp
        .into_body()
        .read_to_string()
        .map_err(|e| format!("read SSE body: {e}"))?;
    sse_result(&text, 2)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sse_result_picks_matching_id_and_skips_control_frames() {
        let body = "data: \nid: 0\nretry: 3000\n\n\
                    data: {\"jsonrpc\":\"2.0\",\"method\":\"notifications/progress\"}\n\n\
                    data: {\"jsonrpc\":\"2.0\",\"id\":2,\"result\":{\"content\":[{\"type\":\"text\",\"text\":\"ok\"}]}}\n\n";
        let result = sse_result(body, 2).expect("matching frame");
        assert_eq!(
            result.pointer("/content/0/text").and_then(Value::as_str),
            Some("ok")
        );
    }

    #[test]
    fn sse_result_surfaces_rpc_errors() {
        let body = "data: {\"jsonrpc\":\"2.0\",\"id\":2,\"error\":{\"code\":-32602,\"message\":\"Unknown tool\"}}";
        let err = sse_result(body, 2).unwrap_err();
        assert!(err.contains("Unknown tool"), "{err}");
    }

    #[test]
    fn sse_result_misses_when_no_frame_matches() {
        let body = "data: {\"jsonrpc\":\"2.0\",\"id\":5,\"result\":{}}";
        assert!(sse_result(body, 2).is_err());
    }
}

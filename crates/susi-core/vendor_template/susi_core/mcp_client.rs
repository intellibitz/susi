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
    // Serialize once so the request signature's body hash is guaranteed
    // byte-identical to the wire payload — the receiver re-hashes what
    // arrived, and a mismatch is an authentication failure, not a quirk.
    let body_bytes = serde_json::to_vec(body).unwrap_or_default();
    // Prove `node.key` possession on every POST — a member whose pubkey is
    // bound in the receiver's roster gets authorized by signature alone,
    // and its sniffable shared bearer is refused (see `net_guard`).
    for (name, value) in signed_headers(url, &body_bytes) {
        req = req.header(&name, value);
    }
    // Seal the body for receivers whose pubkey is bound in our roster.
    // The signature binds the *plaintext* hash, so a relay that strips
    // the enc headers cannot make the ciphertext verify — confidentiality
    // failure fails closed, never downgrades to readable plaintext.
    let mut wire = body_bytes;
    let mut content_type = "application/json";
    let addr = url
        .split_once("://")
        .and_then(|(_, rest)| rest.split('/').next())
        .unwrap_or("");
    if let Some(peer_pk) = crate::susi_config::cluster_key::bound_pubkey_for_addr(addr) {
        if let (Some((nonce, ct)), Some(our_pk)) = (
            crate::susi_config::cluster_key::member_seal(&peer_pk, &wire),
            crate::susi_config::cluster_key::node_pubkey_hex(),
        ) {
            req = req
                .header("x-susi-enc", "v1")
                .header("x-susi-enc-nonce", nonce)
                // Our pubkey travels in the clear so the receiver can
                // still open the body during asymmetric-roster windows
                // (bound on our side, not yet committed on theirs);
                // AEAD verification makes a swapped pubkey a hard
                // failure, and auth still verifies the signature
                // against the *roster-bound* key.
                .header("x-susi-node-pub", our_pk);
            wire = ct;
            content_type = "application/octet-stream";
        }
    }
    req.header("content-type", content_type)
        .send(&wire)
        .map_err(|e| format!("POST {url}: {e}"))
}

/// `X-Susi-*` request-signature headers over
/// `susi-peer-req-v2:{node}:{ts}:{nonce}:{method}:{path}:{sha256(body)}`.
/// Empty when no `node.key` exists (standalone host) — receivers treat
/// the request as an unsigned call and apply the bearer/token rules.
fn signed_headers(url: &str, body_bytes: &[u8]) -> Vec<(String, String)> {
    use crate::susi_config::cluster_key;
    use sha2::Digest;
    let node = cluster_key::wire_node_id();
    let path = url.splitn(4, '/').nth(3).map_or_else(
        || "/".to_string(),
        |p| {
            // url = "http://{addr}/mcp" — the split yields the path sans
            // leading slash; normalize so the receiver's
            // `req.uri().path()` compares equal.
            if p.is_empty() {
                "/".to_string()
            } else {
                format!("/{p}")
            }
        },
    );
    let ts = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0);
    let nonce = cluster_key::random_nonce_hex();
    let hash = hex::encode(sha2::Sha256::digest(body_bytes));
    let canonical = format!("susi-peer-req-v2:{node}:{ts}:{nonce}:POST:{path}:{hash}");
    let Some(sig) = cluster_key::member_sign(&canonical) else {
        return Vec::new();
    };
    vec![
        ("x-susi-node".to_string(), node),
        ("x-susi-req-ts".to_string(), ts.to_string()),
        ("x-susi-req-nonce".to_string(), nonce),
        ("x-susi-req-sig".to_string(), sig),
    ]
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
    session_call(
        addr,
        "tools/call",
        &json!({
            "name": tool,
            "arguments": arguments
        }),
        bearer,
    )
}

/// Run any session-scoped JSON-RPC method (`tools/list`, `tools/call`,
/// `resources/list`, `ping`, ...) through the initialize → initialized →
/// request handshake. Returns the frame's `result` object.
pub fn session_call(
    addr: &str,
    method: &str,
    params: &Value,
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

    // 3. the request — the SSE body carries the response frame.
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
    // Bound the response: a peer streaming an unbounded body inside the
    // 20s recv window could still exhaust memory — cap at 16 MiB, far
    // above any tool result the protocol legitimately carries.
    let text = {
        use std::io::Read;
        let mut buf = String::new();
        resp.into_body()
            .as_reader()
            .take(16 * 1024 * 1024)
            .read_to_string(&mut buf)
            .map_err(|e| format!("read SSE body: {e}"))?;
        buf
    };
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

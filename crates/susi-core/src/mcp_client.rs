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
//!        (or a single `application/json` message — both are spec-valid)
//! DELETE /mcp                            (session headers) -> session closed
//! ```
//!
//! The `MCP-Protocol-Version` header carries the version the server chose in
//! its `initialize` result, and a version this client does not speak aborts
//! the call before any request is sent.
//!
//! Posting `tools/call` directly to `/` or without the session handshake is
//! rejected by the server (404 / "expect initialize request") — the naive
//! single-shot POST this module replaced silently 404'd, which is how peer
//! dispatch and commit replication went dead in production while still
//! "succeeding" from the caller's perspective.

use serde_json::{json, Value};

/// Protocol revision this client requests; matched against
/// `crates/susi-gmcp/src/protocol_tests.rs`.
const PROTOCOL_VERSION: &str = "2025-11-25";

/// Revisions whose `initialize` / `tools/call` shapes this client speaks. A
/// server may answer `initialize` with any version it supports; anything
/// outside this set is a negotiation failure.
const SUPPORTED_VERSIONS: [&str; 3] = [PROTOCOL_VERSION, "2025-06-18", "2025-03-26"];

/// Cap on a response body — far above any tool result the protocol
/// legitimately carries, but a peer streaming an unbounded body inside the
/// 20s recv window cannot exhaust memory.
const MAX_RESPONSE_BYTES: u64 = 16 * 1024 * 1024;

/// Established session: the id the server issued plus the negotiated
/// protocol revision every later request must declare.
struct Session {
    id: String,
    version: String,
}

/// The response plus the peer's bound pubkey when the request went out
/// sealed — `Some` means the peer MUST answer sealed; an unsealed reply
/// is a downgrade attempt, not a format variant.
fn post(
    url: &str,
    bearer: Option<&str>,
    session: Option<&Session>,
    body: &Value,
) -> Result<(ureq::http::Response<ureq::Body>, Option<String>), String> {
    let mut req = crate::susi_config::http_agent()
        .post(url)
        .header("accept", "application/json, text/event-stream");
    if let Some(token) = bearer {
        req = req.header("authorization", format!("Bearer {token}"));
    }
    if let Some(session) = session {
        req = req
            .header("mcp-session-id", &session.id)
            .header("mcp-protocol-version", &session.version);
    }
    // Serialize once so the request signature's body hash is guaranteed
    // byte-identical to the wire payload — the receiver re-hashes what
    // arrived, and a mismatch is an authentication failure, not a quirk.
    let body_bytes = serde_json::to_vec(body).unwrap_or_default();
    // Prove `node.key` possession on every POST — a member whose pubkey is
    // bound in the receiver's roster gets authorized by signature alone,
    // and its sniffable shared bearer is refused (see `net_guard`).
    for (name, value) in signed_headers("POST", url, &body_bytes) {
        req = req.header(&name, value);
    }
    // Seal the body for receivers whose pubkey is bound in our roster.
    // The signature binds the *plaintext* hash, so a relay that strips
    // the enc headers cannot make the ciphertext verify — confidentiality
    // failure fails closed, never downgrades to readable plaintext.
    let mut wire = body_bytes;
    let mut content_type = "application/json";
    let mut sealed_to = None;
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
            sealed_to = Some(peer_pk);
        }
    }
    req.header("content-type", content_type)
        .send(&wire)
        .map(|r| (r, sealed_to))
        .map_err(|e| format!("POST {url}: {e}"))
}

/// Best-effort `DELETE` of a finished session (spec: clients SHOULD
/// terminate sessions they no longer need). A 405 — server does not allow
/// client-initiated termination — or a transport error only means the
/// server reclaims the session on its own idle timeout.
fn delete_session(url: &str, bearer: Option<&str>, session: &Session) {
    let mut req = crate::susi_config::http_agent()
        .delete(url)
        .header("mcp-session-id", &session.id)
        .header("mcp-protocol-version", &session.version);
    if let Some(token) = bearer {
        req = req.header("authorization", format!("Bearer {token}"));
    }
    for (name, value) in signed_headers("DELETE", url, &[]) {
        req = req.header(&name, value);
    }
    let _ = req.call();
}

/// `X-Susi-*` request-signature headers over
/// `susi-peer-req-v2:{node}:{ts}:{nonce}:{method}:{path}:{sha256(body)}`.
/// Empty when no `node.key` exists (standalone host) — receivers treat
/// the request as an unsigned call and apply the bearer/token rules.
fn signed_headers(method: &str, url: &str, body_bytes: &[u8]) -> Vec<(String, String)> {
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
    let canonical = format!("susi-peer-req-v2:{node}:{ts}:{nonce}:{method}:{path}:{hash}");
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

/// The `result` of the JSON-RPC response `msg` if it answers `id`; `None`
/// for notifications, server requests, and other ids.
fn match_response(msg: &Value, id: u64) -> Option<Result<Value, String>> {
    if msg.get("id").and_then(Value::as_u64) != Some(id) || msg.get("method").is_some() {
        return None;
    }
    if let Some(err) = msg.get("error") {
        return Some(Err(format!("peer rpc error: {err}")));
    }
    Some(
        msg.get("result")
            .cloned()
            .ok_or_else(|| "response carries neither result nor error".to_string()),
    )
}

/// Extract the JSON-RPC response for `id` from an SSE body. Per the SSE
/// spec an event's `data:` lines join with `\n` and a blank line
/// dispatches it; priming events (empty data, `id:`/`retry:` only) and
/// unrelated notifications are skipped.
fn sse_result(body: &str, id: u64) -> Result<Value, String> {
    let mut data = String::new();
    for line in body.lines().chain(std::iter::once("")) {
        if line.is_empty() {
            if let Ok(msg) = serde_json::from_str::<Value>(&data) {
                if let Some(result) = match_response(&msg, id) {
                    return result;
                }
            }
            data.clear();
        } else if let Some(field) = line.strip_prefix("data:") {
            if !data.is_empty() {
                data.push('\n');
            }
            data.push_str(field.strip_prefix(' ').unwrap_or(field));
        }
    }
    Err("no matching response frame in SSE stream".to_string())
}

/// Response `id` from either spec-valid body shape: a single
/// `application/json` message or a `text/event-stream`. Sniffed from the
/// content rather than the header because sealed replies travel as
/// `application/octet-stream`.
fn response_result(text: &str, id: u64) -> Result<Value, String> {
    match serde_json::from_str::<Value>(text) {
        Ok(msg) => match_response(&msg, id)
            .unwrap_or_else(|| Err("JSON response does not answer the request id".to_string())),
        Err(_) => sse_result(text, id),
    }
}

/// Read a bounded response body, opening it when the request went out
/// sealed — an unsealed reply to a sealed request means the channel was
/// downgraded on-path and is refused.
fn read_body(
    resp: ureq::http::Response<ureq::Body>,
    sealed: Option<&str>,
) -> Result<String, String> {
    let header = |name: &str| {
        resp.headers()
            .get(name)
            .and_then(|v| v.to_str().ok())
            .map(str::to_string)
    };
    let enc_headers = (header("x-susi-enc"), header("x-susi-enc-nonce"));
    let body = {
        use std::io::Read;
        let mut buf = Vec::new();
        resp.into_body()
            .as_reader()
            .take(MAX_RESPONSE_BYTES + 1)
            .read_to_end(&mut buf)
            .map_err(|e| format!("read response body: {e}"))?;
        buf
    };
    let Some(pk) = sealed else {
        return String::from_utf8(body).map_err(|e| format!("response body not utf-8: {e}"));
    };
    let (Some(v), Some(nonce)) = enc_headers else {
        return Err("sealed request answered by an unsealed response".to_string());
    };
    if v != "v1" {
        return Err(format!("unknown sealed-response version {v}"));
    }
    let pt = crate::susi_config::cluster_key::member_open(pk, &nonce, &body)
        .ok_or_else(|| "sealed response failed AEAD verification".to_string())?;
    String::from_utf8(pt).map_err(|e| format!("sealed response not utf-8: {e}"))
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

    // 1. initialize — the response header carries the session id and the
    //    result carries the negotiated protocol revision.
    let (init, sealed) = post(
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
    // A sealed request demands a sealed response — the session id in
    // this reply is a bearer-grade credential; plaintext carriage of
    // it to a sealed request means a relay is stripping the channel.
    if sealed.is_some() && init.headers().get("x-susi-enc").is_none() {
        return Err(format!(
            "peer {addr} answered a sealed request in plaintext"
        ));
    }
    let id = init
        .headers()
        .get("mcp-session-id")
        .and_then(|v| v.to_str().ok())
        .map(str::to_string)
        .ok_or_else(|| format!("peer {addr} returned no Mcp-Session-Id"))?;
    let init_result = response_result(&read_body(init, sealed.as_deref())?, 1);
    let session = Session {
        version: String::new(),
        id,
    };
    let version = match init_result.and_then(|result| negotiated_version(&result)) {
        Ok(version) => version,
        Err(e) => {
            delete_session(&url, bearer, &session);
            return Err(format!("peer {addr}: {e}"));
        }
    };
    let session = Session { version, ..session };

    // 2. notifications/initialized — required before the session accepts
    //    further messages; 3. the request; then release the session.
    let result = post(
        &url,
        bearer,
        Some(&session),
        &json!({"jsonrpc": "2.0", "method": "notifications/initialized"}),
    )
    .and_then(|_| session_request(&url, bearer, &session, method, params));
    delete_session(&url, bearer, &session);
    result
}

/// The server's chosen revision from an `initialize` result, refused when
/// this client does not speak it.
fn negotiated_version(result: &Value) -> Result<String, String> {
    let version = result
        .get("protocolVersion")
        .and_then(Value::as_str)
        .ok_or_else(|| "initialize result carries no protocolVersion".to_string())?;
    if SUPPORTED_VERSIONS.contains(&version) {
        Ok(version.to_string())
    } else {
        Err(format!("unsupported MCP protocol version {version}"))
    }
}

/// POST one request inside an established session and parse the matching
/// response message.
fn session_request(
    url: &str,
    bearer: Option<&str>,
    session: &Session,
    method: &str,
    params: &Value,
) -> Result<Value, String> {
    let (resp, sealed) = post(
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
    response_result(&read_body(resp, sealed.as_deref())?, 2)
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

    #[test]
    fn sse_result_joins_multiline_data_fields() {
        let body = "data: {\"jsonrpc\":\"2.0\",\ndata: \"id\":2,\"result\":{\"ok\":true}}\n\n";
        assert_eq!(sse_result(body, 2).unwrap(), json!({"ok": true}));
    }

    #[test]
    fn response_result_accepts_plain_json_bodies() {
        let body = r#"{"jsonrpc":"2.0","id":2,"result":{"tools":[]}}"#;
        assert_eq!(response_result(body, 2).unwrap(), json!({"tools": []}));
        assert!(response_result(r#"{"jsonrpc":"2.0","id":9,"result":{}}"#, 2).is_err());
    }

    #[test]
    fn server_requests_sharing_an_id_are_not_responses() {
        let body = "data: {\"jsonrpc\":\"2.0\",\"id\":2,\"method\":\"elicitation/create\"}\n\n\
                    data: {\"jsonrpc\":\"2.0\",\"id\":2,\"result\":{}}\n\n";
        assert_eq!(sse_result(body, 2).unwrap(), json!({}));
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
}

//! Black-box conformance suite for the Susi host contract.
//!
//! Products that plug into Susi hard-code the five contract ports (README,
//! "Host contract"). This suite checks a running host the way such a product
//! sees it: liveness without credentials, bearer authentication on every HTTP
//! port, the MCP Streamable HTTP handshake, A2A discovery and JSON-RPC errors,
//! and the GEMI model list. Every check records its own wall-clock latency.
#![cfg_attr(test, allow(clippy::unwrap_used, clippy::expect_used))]

pub mod http;

use serde_json::{json, Value};
use std::time::{Duration, Instant};

/// Contract ports before `port_offset` is applied.
pub mod ports {
    /// GMCP / MCP HTTP (`/mcp`).
    pub const MCP: u16 = 9090;
    /// GEMI HTTP (inference, models).
    pub const GEMI: u16 = 9091;
    /// GMCP HTTP alias (streamable / SSE).
    pub const MCP_ALIAS: u16 = 9093;
    /// A2A HTTP (JSON-RPC, agent card).
    pub const A2A: u16 = 9094;
}

/// Current MCP revision: stateless requests named by the `Mcp-Method` header.
pub const MCP_CURRENT: &str = "2026-07-28";
/// Session-based MCP revision most deployed clients still speak.
pub const MCP_SESSION: &str = "2025-06-18";

/// Where and how to reach the host under test.
#[derive(Debug, Clone)]
pub struct Target {
    /// Host name or address (normally `127.0.0.1`).
    pub host: String,
    /// The host's `port_offset`; contract ports shift together.
    pub port_offset: u16,
    /// Bearer token (`~/.susi/api_token`); authenticated checks need it.
    pub token: Option<String>,
    /// Per-request timeout.
    pub timeout: Duration,
}

/// Outcome of one contract check.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Outcome {
    /// The host meets the contract.
    Pass,
    /// The host violates the contract.
    Fail,
    /// Allowed by the contract, but integrators should know.
    Warn,
    /// Not run (for example, no token was available).
    Skip,
}

impl Outcome {
    /// Lower-case label used in reports.
    #[must_use]
    pub fn label(self) -> &'static str {
        match self {
            Self::Pass => "pass",
            Self::Fail => "FAIL",
            Self::Warn => "warn",
            Self::Skip => "skip",
        }
    }
}

/// One check's result.
#[derive(Debug, Clone)]
pub struct Check {
    /// Stable identifier, e.g. `mcp.initialize@9090`.
    pub id: String,
    /// Result.
    pub outcome: Outcome,
    /// Wall-clock latency of the request(s), in milliseconds.
    pub millis: u128,
    /// What was observed.
    pub detail: String,
}

impl Check {
    /// JSON form for machine consumers.
    #[must_use]
    pub fn to_json(&self) -> Value {
        json!({
            "id": self.id,
            "outcome": self.outcome.label().to_ascii_lowercase(),
            "ms": self.millis,
            "detail": self.detail,
        })
    }
}

impl Target {
    fn port(&self, base: u16) -> u16 {
        base.saturating_add(self.port_offset)
    }

    fn call(&self, base: u16, req: Call<'_>) -> Result<http::Response, String> {
        let bearer = self.token.as_ref().map(|t| format!("Bearer {t}"));
        let mut headers: Vec<(&str, &str)> = req.headers.to_vec();
        if req.auth {
            if let Some(value) = bearer.as_deref() {
                headers.push(("Authorization", value));
            }
        }
        let payload = req.body.map(Value::to_string);
        if payload.is_some() {
            headers.push(("Content-Type", "application/json"));
        }
        http::request(
            &self.host,
            self.port(base),
            http::Request {
                method: req.method,
                path: req.path,
                headers: &headers,
                body: payload.as_deref().map(str::as_bytes),
            },
            self.timeout,
        )
    }
}

/// A contract request before authentication and encoding.
#[derive(Clone, Copy)]
struct Call<'a> {
    method: &'a str,
    path: &'a str,
    auth: bool,
    headers: &'a [(&'a str, &'a str)],
    body: Option<&'a Value>,
}

impl<'a> Call<'a> {
    fn get(path: &'a str, auth: bool) -> Self {
        Self {
            method: "GET",
            path,
            auth,
            headers: &[],
            body: None,
        }
    }
    fn post(path: &'a str, auth: bool, headers: &'a [(&'a str, &'a str)], body: &'a Value) -> Self {
        Self {
            method: "POST",
            path,
            auth,
            headers,
            body: Some(body),
        }
    }
}

/// JSON-RPC payload from a plain JSON body or the last JSON-RPC `data:` event
/// of an SSE body (MCP Streamable HTTP may answer either way).
#[must_use]
pub fn rpc_payload(response: &http::Response) -> Option<Value> {
    let text = response.text();
    if let Ok(value) = serde_json::from_str::<Value>(text.trim()) {
        return Some(value);
    }
    text.lines()
        .filter_map(|line| line.strip_prefix("data:"))
        .filter_map(|data| serde_json::from_str::<Value>(data.trim()).ok())
        .rfind(|value| value.get("jsonrpc").is_some())
}

fn timed(id: String, run: impl FnOnce() -> (Outcome, String)) -> Check {
    let started = Instant::now();
    let (outcome, detail) = run();
    Check {
        id,
        outcome,
        millis: started.elapsed().as_millis(),
        detail,
    }
}

fn verdict(ok: bool, detail: String) -> (Outcome, String) {
    (if ok { Outcome::Pass } else { Outcome::Fail }, detail)
}

/// Liveness must not require credentials (load balancers, supervisors).
fn health(target: &Target, base: u16) -> Check {
    timed(format!("health@{}", target.port(base)), || {
        match target.call(base, Call::get("/health", false)) {
            Ok(r) => verdict(
                r.status == 200,
                format!("GET /health without token -> {}", r.status),
            ),
            Err(e) => (Outcome::Fail, e),
        }
    })
}

/// A protected request without a token must be refused with 401.
fn auth_required(target: &Target, base: u16, method: &str, path: &str) -> Check {
    timed(format!("auth.required@{}", target.port(base)), || {
        let body = (method == "POST").then(|| json!({"jsonrpc":"2.0","id":1,"method":"ping"}));
        match target.call(
            base,
            Call {
                method,
                path,
                auth: false,
                headers: &[],
                body: body.as_ref(),
            },
        ) {
            Ok(r) => verdict(
                r.status == 401,
                format!("{method} {path} without token -> {}", r.status),
            ),
            Err(e) => (Outcome::Fail, e),
        }
    })
}

fn needs_token(target: &Target, id: &str) -> Option<Check> {
    target.token.is_none().then(|| Check {
        id: id.to_string(),
        outcome: Outcome::Skip,
        millis: 0,
        detail: "no bearer token available".into(),
    })
}

const MCP_ACCEPT: (&str, &str) = ("Accept", "application/json, text/event-stream");

/// One stateless request at the current MCP revision.
fn mcp_current(target: &Target, base: u16, id: i64, method: &str) -> Result<(u16, Value), String> {
    let request = json!({"jsonrpc":"2.0","id":id,"method":method,"params":{"_meta":{
        "io.modelcontextprotocol/protocolVersion": MCP_CURRENT,
        "io.modelcontextprotocol/clientCapabilities": {}}}});
    let headers = [
        MCP_ACCEPT,
        ("MCP-Protocol-Version", MCP_CURRENT),
        ("Mcp-Method", method),
    ];
    let r = target.call(base, Call::post("/mcp", true, &headers, &request))?;
    Ok((r.status, rpc_payload(&r).unwrap_or(Value::Null)))
}

fn mcp_discover(target: &Target, base: u16) -> Check {
    let id = format!("mcp.discover@{}", target.port(base));
    if let Some(skip) = needs_token(target, &id) {
        return skip;
    }
    timed(id, || {
        match mcp_current(target, base, 1, "server/discover") {
            Ok((status, payload)) => {
                let result = &payload["result"];
                let server = result["_meta"]["io.modelcontextprotocol/serverInfo"]["name"]
                    .as_str()
                    .unwrap_or("");
                verdict(
                    status == 200 && result["capabilities"].is_object() && !server.is_empty(),
                    format!("status {status} server {server:?} ({MCP_CURRENT})"),
                )
            }
            Err(e) => (Outcome::Fail, e),
        }
    })
}

fn mcp_tools(target: &Target) -> Check {
    let id = format!("mcp.tools-list@{}", target.port(ports::MCP));
    if let Some(skip) = needs_token(target, &id) {
        return skip;
    }
    timed(id, || {
        match mcp_current(target, ports::MCP, 2, "tools/list") {
            Ok((status, payload)) => {
                let tools = payload["result"]["tools"].as_array().map(Vec::len);
                verdict(
                    status == 200 && tools.is_some(),
                    format!("status {status}, {} tools", tools.unwrap_or(0)),
                )
            }
            Err(e) => (Outcome::Fail, e),
        }
    })
}

fn mcp_unknown(target: &Target) -> Check {
    let id = format!("mcp.unknown-method@{}", target.port(ports::MCP));
    if let Some(skip) = needs_token(target, &id) {
        return skip;
    }
    timed(id, || {
        match mcp_current(target, ports::MCP, 3, "susi/conformance-unknown-method") {
            Ok((status, payload)) => {
                let code = payload["error"]["code"].as_i64();
                verdict(
                    code == Some(-32601),
                    format!("status {status}, JSON-RPC error code {code:?} (want -32601)"),
                )
            }
            Err(e) => (Outcome::Fail, e),
        }
    })
}

/// The session flow of MCP 2025-06-18: `initialize`, `notifications/initialized`,
/// then `tools/list` on the same `Mcp-Session-Id`.
fn mcp_session(target: &Target) -> Check {
    let id = format!("mcp.session-{MCP_SESSION}@{}", target.port(ports::MCP));
    if let Some(skip) = needs_token(target, &id) {
        return skip;
    }
    timed(id, || {
        let init = json!({"jsonrpc":"2.0","id":1,"method":"initialize","params":{
            "protocolVersion": MCP_SESSION, "capabilities": {},
            "clientInfo": {"name":"susi-conformance","version": env!("CARGO_PKG_VERSION")}}});
        let r = match target.call(ports::MCP, Call::post("/mcp", true, &[MCP_ACCEPT], &init)) {
            Ok(r) => r,
            Err(e) => return (Outcome::Fail, format!("initialize: {e}")),
        };
        let negotiated = rpc_payload(&r).unwrap_or(Value::Null)["result"]["protocolVersion"]
            .as_str()
            .unwrap_or("")
            .to_string();
        let Some(session) = r.header("mcp-session-id").map(str::to_string) else {
            return (
                Outcome::Fail,
                format!("initialize {} without Mcp-Session-Id", r.status),
            );
        };
        let headers = [
            MCP_ACCEPT,
            ("MCP-Protocol-Version", negotiated.as_str()),
            ("Mcp-Session-Id", session.as_str()),
        ];
        let note = json!({"jsonrpc":"2.0","method":"notifications/initialized"});
        let noted = target
            .call(ports::MCP, Call::post("/mcp", true, &headers, &note))
            .map(|r| r.status);
        let list = json!({"jsonrpc":"2.0","id":2,"method":"tools/list","params":{}});
        match target.call(ports::MCP, Call::post("/mcp", true, &headers, &list)) {
            Ok(r) => {
                let tools = rpc_payload(&r).unwrap_or(Value::Null)["result"]["tools"]
                    .as_array()
                    .map(Vec::len);
                verdict(
                    r.status == 200 && tools.is_some(),
                    format!(
                        "negotiated {negotiated:?}; initialized -> {noted:?}; tools/list -> {} ({} tools)",
                        r.status,
                        tools.unwrap_or(0)
                    ),
                )
            }
            Err(e) => (
                Outcome::Fail,
                format!("initialized -> {noted:?}; tools/list: {e}"),
            ),
        }
    })
}

fn a2a_card(target: &Target) -> Check {
    timed(
        format!("a2a.agent-card@{}", target.port(ports::A2A)),
        || match target.call(ports::A2A, Call::get("/.well-known/agent-card.json", false)) {
            Ok(r) => {
                let card: Value = serde_json::from_slice(&r.body).unwrap_or(Value::Null);
                let name = card["name"].as_str().unwrap_or("");
                let url = card["supportedInterfaces"][0]["url"].as_str().unwrap_or("");
                verdict(
                    r.status == 200 && !name.is_empty() && !url.is_empty(),
                    format!("status {} name {name:?} interface {url:?}", r.status),
                )
            }
            Err(e) => (Outcome::Fail, e),
        },
    )
}

fn a2a_unknown(target: &Target) -> Check {
    let id = format!("a2a.unknown-method@{}", target.port(ports::A2A));
    if let Some(skip) = needs_token(target, &id) {
        return skip;
    }
    timed(id, || {
        let request =
            json!({"jsonrpc":"2.0","id":7,"method":"susi/conformance-unknown-method","params":{}});
        match target.call(ports::A2A, Call::post("/", true, &[], &request)) {
            Ok(r) => {
                let payload = rpc_payload(&r).unwrap_or(Value::Null);
                let code = payload["error"]["code"].as_i64();
                verdict(
                    code == Some(-32601),
                    format!(
                        "status {}, JSON-RPC error code {code:?} (want -32601)",
                        r.status
                    ),
                )
            }
            Err(e) => (Outcome::Fail, e),
        }
    })
}

/// True for ids that disclose a local filesystem location.
#[must_use]
pub fn is_local_path(id: &str) -> bool {
    let bytes = id.as_bytes();
    id.starts_with('/')
        || id.starts_with("\\\\")
        || (bytes.len() > 2 && bytes[1] == b':' && (bytes[2] == b'\\' || bytes[2] == b'/'))
}

fn gemi_models(target: &Target) -> Check {
    let id = format!("gemi.models@{}", target.port(ports::GEMI));
    if let Some(skip) = needs_token(target, &id) {
        return skip;
    }
    timed(id, || {
        match target.call(ports::GEMI, Call::get("/v1/models", true)) {
            Ok(r) => {
                let list: Value = serde_json::from_slice(&r.body).unwrap_or(Value::Null);
                let Some(models) = list["data"].as_array() else {
                    return (
                        Outcome::Fail,
                        format!("status {}, no `data` array", r.status),
                    );
                };
                let leaking = models
                    .iter()
                    .filter(|m| m["id"].as_str().is_some_and(is_local_path))
                    .count();
                if r.status != 200 {
                    (Outcome::Fail, format!("status {}", r.status))
                } else if leaking > 0 {
                    (
                        Outcome::Warn,
                        format!(
                            "{} models; {leaking} ids are local filesystem paths",
                            models.len()
                        ),
                    )
                } else {
                    (Outcome::Pass, format!("{} models", models.len()))
                }
            }
            Err(e) => (Outcome::Fail, e),
        }
    })
}

/// Run every contract check against `target`, in a stable order.
#[must_use]
pub fn run(target: &Target) -> Vec<Check> {
    let mut checks = Vec::new();
    for base in [ports::MCP, ports::GEMI, ports::MCP_ALIAS, ports::A2A] {
        checks.push(health(target, base));
    }
    checks.push(auth_required(target, ports::MCP, "POST", "/mcp"));
    checks.push(auth_required(target, ports::GEMI, "GET", "/v1/models"));
    checks.push(auth_required(target, ports::MCP_ALIAS, "POST", "/mcp"));
    checks.push(auth_required(target, ports::A2A, "POST", "/"));
    checks.push(mcp_discover(target, ports::MCP));
    checks.push(mcp_discover(target, ports::MCP_ALIAS));
    checks.push(mcp_tools(target));
    checks.push(mcp_unknown(target));
    checks.push(mcp_session(target));
    checks.push(a2a_card(target));
    checks.push(a2a_unknown(target));
    checks.push(gemi_models(target));
    checks
}

/// True when no check failed (warnings and skips do not fail the suite).
#[must_use]
pub fn conformant(checks: &[Check]) -> bool {
    checks.iter().all(|c| c.outcome != Outcome::Fail)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sse_and_plain_rpc_payloads_parse() {
        let sse = http::Response {
            status: 200,
            headers: vec![],
            body: b"data: \nid: 0\n\ndata: {\"jsonrpc\":\"2.0\",\"id\":1,\"result\":{}}\n\n"
                .to_vec(),
        };
        assert_eq!(rpc_payload(&sse).unwrap()["id"], 1);
        let plain = http::Response {
            status: 200,
            headers: vec![],
            body: br#"{"jsonrpc":"2.0","id":2,"error":{"code":-32601}}"#.to_vec(),
        };
        assert_eq!(rpc_payload(&plain).unwrap()["error"]["code"], -32601);
    }

    #[test]
    fn local_paths_are_recognized() {
        for id in [
            "C:\\Users\\a\\m.gguf",
            "/home/a/m.gguf",
            "\\\\srv\\m",
            "d:/m",
        ] {
            assert!(is_local_path(id), "{id}");
        }
        for id in [
            "qwen2.5-0.5b-instruct-q4_0",
            "catalog-deepseek-chat",
            "org/model",
        ] {
            assert!(!is_local_path(id), "{id}");
        }
    }

    #[test]
    fn unreachable_host_fails_closed_and_skips_without_token() {
        // Contract ports shifted far above anything a test host serves.
        let checks = run(&Target {
            host: "127.0.0.1".into(),
            port_offset: 50_000,
            token: None,
            timeout: Duration::from_millis(200),
        });
        assert!(!conformant(&checks));
        assert!(checks.iter().any(|c| c.outcome == Outcome::Skip));
        assert!(checks.iter().all(|c| c.outcome != Outcome::Pass));
    }
}

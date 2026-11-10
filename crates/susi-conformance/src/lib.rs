//! Black-box conformance suite for the Susi host contract.
//!
//! Products that plug into Susi hard-code the five contract ports (README,
//! "Host contract"). This suite checks a running host the way such a product
//! sees it: liveness without credentials, bearer authentication on every HTTP
//! port, the MCP Streamable HTTP handshake, A2A and ACP discovery and JSON-RPC
//! routes on 9094, UDP discovery on 9092, the GEMI model list, and, when
//! asked, a streamed GEMI completion and MCP/ACP over stdio (`susi mcp serve`,
//! `susi acp serve`). Every check records its own wall-clock latency.
#![forbid(unsafe_code)]
#![cfg_attr(test, allow(clippy::unwrap_used, clippy::expect_used))]

pub use susi_wire_client::http;

use serde_json::{json, Value};
use std::io::{BufRead, BufReader, Write};
use std::path::PathBuf;
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

/// Contract ports before `port_offset` is applied, plus the explicit
/// out-of-contract internal port and IPC inventory (`B1`).
pub mod ports {
    /// GMCP / MCP HTTP (`/mcp`).
    pub const MCP: u16 = 9090;
    /// GEMI HTTP (inference, models).
    pub const GEMI: u16 = 9091;
    /// A2A UDP discovery: answers cluster-key signed pings only.
    pub const UDP_DISCOVERY: u16 = 9092;
    /// GMCP HTTP alias (streamable / SSE). A permanent alias of 9090:
    /// deployed clients hard-code it.
    pub const MCP_ALIAS: u16 = 9093;
    /// A2A + ACP HTTP (JSON-RPC, SSE, agent card, ACP discovery).
    pub const A2A: u16 = 9094;

    /// The five stable host-contract base ports.
    pub const CONTRACT_PORTS: [u16; 5] = [MCP, GEMI, UDP_DISCOVERY, MCP_ALIAS, A2A];

    /// Explicit out-of-contract internal TCP/UDP ports (`B1`). External
    /// products must never couple to these internal substrate ports.
    pub const OUT_OF_CONTRACT_PORTS: &[(u16, &str)] = &[
        (9095, "gossip"),
        (9180, "gateway"),
        (9181, "harness-api"),
        (18080, "leaf-paths"),
        (18081, "leaf-error"),
        (18082, "leaf-config"),
        (18083, "leaf-sandbox"),
        (18084, "leaf-transport"),
        (19090, "cell-gemi"),
        (19091, "cell-gmcp"),
        (19092, "cell-gawd"),
        (19093, "cell-universal"),
    ];

    /// Explicit out-of-contract local IPC surfaces (`B1`).
    pub const OUT_OF_CONTRACT_IPC: &[&str] = &["plane_bus"];

    /// True when `base_port` is one of the five public host-contract ports.
    #[must_use]
    pub fn is_contract_port(base_port: u16) -> bool {
        CONTRACT_PORTS.contains(&base_port)
    }

    /// Internal role name when `base_port` is on the out-of-contract list.
    #[must_use]
    pub fn out_of_contract_role(base_port: u16) -> Option<&'static str> {
        OUT_OF_CONTRACT_PORTS
            .iter()
            .find_map(|&(port, role)| (port == base_port).then_some(role))
    }
}

pub use ports::{
    is_contract_port, out_of_contract_role, CONTRACT_PORTS, OUT_OF_CONTRACT_IPC,
    OUT_OF_CONTRACT_PORTS,
};

/// Current MCP revision: stateless requests named by the `Mcp-Method` header.
pub const MCP_CURRENT: &str = "2026-07-28";
/// Session-based MCP revision most deployed clients still speak.
pub const MCP_SESSION: &str = "2025-06-18";
/// Canonical ACP discovery document schema identifier on `/.well-known/acp.json`.
pub const ACP_DISCOVERY_SCHEMA: &str = "susi-acp-discovery/v1";
/// Canonical ACP wire protocol version.
pub const ACP_PROTOCOL: &str = "2026-11-11";

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

/// Checks that do more than read the host, so they run only when asked.
#[derive(Debug, Clone, Default)]
pub struct Optional {
    /// `susi` binary for the MCP and ACP stdio checks (`<bin> mcp serve`,
    /// `<bin> acp serve`). Each child inherits this process's environment, so
    /// `SUSI_HOME` selects the instance; it is always stopped before the check
    /// returns.
    pub susi_bin: Option<PathBuf>,
    /// Run the GEMI SSE check: one streamed, 1-token, local-only
    /// (`susi.allow_cloud: false`) chat completion. Costs a local inference.
    pub inference: bool,
    /// Pre-signed `SUSI_PING_SIG*` datagram for opt-in positive UDP
    /// discovery verification (`SUSI_PONG_SIG*`).
    pub signed_udp_ping: Option<String>,
    /// Optional required prefix on the positive UDP discovery pong reply
    /// (for example `"SUSI_PONG_SIG2:"`).
    pub signed_udp_pong_prefix: Option<String>,
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
    auth_required_as(target, base, method, path, "")
}

/// [`auth_required`] with `suffix` appended to the check id, so a second
/// route on the same port (`/stream`) gets its own id.
fn auth_required_as(target: &Target, base: u16, method: &str, path: &str, suffix: &str) -> Check {
    timed(
        format!("auth.required@{}{suffix}", target.port(base)),
        || {
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
        },
    )
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

/// Upper bound on `tools/list` pages one listing follows.
const MAX_LIST_PAGES: usize = 50;

/// Every tool across a paginated `tools/list`. `fetch(page, cursor)` returns
/// the JSON-RPC payload for one page (`cursor` is `None` for the first);
/// pages are followed until `nextCursor` is absent, at most
/// [`MAX_LIST_PAGES`] of them. A JSON-RPC error (such as a stale cursor), a
/// page without a `tools` array, a non-string cursor, or a server that keeps
/// paging is a clear error naming the page.
fn list_all_tools(
    mut fetch: impl FnMut(usize, Option<&str>) -> Result<Value, String>,
) -> Result<Vec<Value>, String> {
    let mut tools = Vec::new();
    let mut cursor: Option<String> = None;
    for page in 0..MAX_LIST_PAGES {
        let payload = fetch(page, cursor.as_deref())?;
        if let Some(error) = payload.get("error") {
            return Err(format!(
                "tools/list page {}: JSON-RPC error {} {}",
                page + 1,
                error["code"],
                error["message"].as_str().unwrap_or("")
            ));
        }
        let Some(items) = payload["result"]["tools"].as_array() else {
            return Err(format!("tools/list page {}: no tools array", page + 1));
        };
        tools.extend(items.iter().cloned());
        cursor = match &payload["result"]["nextCursor"] {
            Value::Null => return Ok(tools),
            Value::String(next) => Some(next.clone()),
            other @ (Value::Bool(_) | Value::Number(_) | Value::Array(_) | Value::Object(_)) => {
                return Err(format!(
                    "tools/list page {}: non-string nextCursor {other}",
                    page + 1
                ))
            }
        };
    }
    Err(format!(
        "tools/list still paging after {MAX_LIST_PAGES} pages"
    ))
}

/// `tools/list` params for one page: empty for the first, the cursor after.
fn list_params(cursor: Option<&str>) -> Value {
    cursor.map_or_else(|| json!({}), |cursor| json!({"cursor": cursor}))
}

/// JSON-RPC id for `tools/list` page `page`, starting at `first`.
fn page_id(first: i64, page: usize) -> i64 {
    first.saturating_add(i64::try_from(page).unwrap_or(i64::MAX))
}

/// One stateless request at the current MCP revision.
fn mcp_current(target: &Target, base: u16, id: i64, method: &str) -> Result<(u16, Value), String> {
    mcp_current_with(target, base, id, method, json!({}))
}

/// One stateless request at the current MCP revision with extra `params`
/// (such as a `tools/list` cursor) beside the revision `_meta`.
fn mcp_current_with(
    target: &Target,
    base: u16,
    id: i64,
    method: &str,
    mut params: Value,
) -> Result<(u16, Value), String> {
    params["_meta"] = json!({
        "io.modelcontextprotocol/protocolVersion": MCP_CURRENT,
        "io.modelcontextprotocol/clientCapabilities": {}});
    let request = json!({"jsonrpc":"2.0","id":id,"method":method,"params":params});
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

fn mcp_tools_at(target: &Target, base: u16) -> Check {
    let id = format!("mcp.tools-list@{}", target.port(base));
    if let Some(skip) = needs_token(target, &id) {
        return skip;
    }
    timed(id, || {
        let listed = list_all_tools(|page, cursor| {
            let (status, payload) = mcp_current_with(
                target,
                base,
                page_id(2, page),
                "tools/list",
                list_params(cursor),
            )?;
            if status == 200 {
                Ok(payload)
            } else {
                Err(format!("tools/list page {}: status {status}", page + 1))
            }
        });
        match listed {
            Ok(tools) => verdict(true, format!("status 200, {} tools", tools.len())),
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
fn mcp_session_at(target: &Target, base: u16) -> Check {
    let id = format!("mcp.session-{MCP_SESSION}@{}", target.port(base));
    if let Some(skip) = needs_token(target, &id) {
        return skip;
    }
    timed(id, || {
        let init = json!({"jsonrpc":"2.0","id":1,"method":"initialize","params":{
            "protocolVersion": MCP_SESSION, "capabilities": {},
            "clientInfo": {"name":"susi-conformance","version": env!("CARGO_PKG_VERSION")}}});
        let r = match target.call(base, Call::post("/mcp", true, &[MCP_ACCEPT], &init)) {
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
            .call(base, Call::post("/mcp", true, &headers, &note))
            .map(|r| r.status);
        let listed = list_all_tools(|page, cursor| {
            let list = json!({"jsonrpc":"2.0","id":page_id(2, page),"method":"tools/list",
                "params":list_params(cursor)});
            let r = target.call(base, Call::post("/mcp", true, &headers, &list))?;
            if r.status == 200 {
                Ok(rpc_payload(&r).unwrap_or(Value::Null))
            } else {
                Err(format!("tools/list page {}: status {}", page + 1, r.status))
            }
        });
        match listed {
            Ok(tools) => verdict(
                true,
                format!(
                    "negotiated {negotiated:?}; initialized -> {noted:?}; tools/list -> 200 ({} tools)",
                    tools.len()
                ),
            ),
            Err(e) => (
                Outcome::Fail,
                format!("initialized -> {noted:?}; tools/list: {e}"),
            ),
        }
    })
}

/// Validate A2A agent card identity, streaming capability, per-capability
/// skills, and honesty (`B6` / `PSP-05` / `PSP-11`).
///
/// # Errors
///
/// Returns the first contract or honesty violation found in `card`.
pub fn validate_agent_card_honesty(card: &Value) -> Result<usize, String> {
    let name = card["name"].as_str().unwrap_or("");
    if name.is_empty() {
        return Err("missing or empty card `name`".into());
    }
    let url = card["supportedInterfaces"][0]["url"].as_str().unwrap_or("");
    if url.is_empty() {
        return Err("missing or empty `supportedInterfaces[0].url`".into());
    }
    if card["capabilities"]["streaming"].as_bool() != Some(true) {
        return Err("`capabilities.streaming` must be true".into());
    }
    let Some(skills) = card["skills"].as_array() else {
        return Err("missing `skills` array".into());
    };
    if skills.is_empty() {
        return Err("`skills` array must not be empty".into());
    }
    for skill in skills {
        let id = skill["id"].as_str().unwrap_or("");
        let skill_name = skill["name"].as_str().unwrap_or("");
        let desc = skill["description"].as_str().unwrap_or("");
        if id.is_empty() || skill_name.is_empty() || desc.is_empty() {
            return Err(format!(
                "skill entry has empty id/name/description (id={id:?})"
            ));
        }
        let desc_lower = desc.to_ascii_lowercase();
        if desc_lower.contains("egui") || desc_lower.contains("desktop ui") {
            return Err(format!(
                "dishonest skill {id:?} claims desktop UI/egui capability: {desc:?}"
            ));
        }
    }
    for required in ["missions", "agents", "frameworks", "models"] {
        if !skills
            .iter()
            .any(|skill| skill["id"].as_str() == Some(required))
        {
            return Err(format!("missing required capability skill {required:?}"));
        }
    }
    Ok(skills.len())
}

fn a2a_card(target: &Target) -> Check {
    timed(
        format!("a2a.agent-card@{}", target.port(ports::A2A)),
        || match target.call(ports::A2A, Call::get("/.well-known/agent-card.json", false)) {
            Ok(r) => {
                let card: Value = serde_json::from_slice(&r.body).unwrap_or(Value::Null);
                let name = card["name"].as_str().unwrap_or("");
                let url = card["supportedInterfaces"][0]["url"].as_str().unwrap_or("");
                match validate_agent_card_honesty(&card) {
                    Ok(skill_count) => verdict(
                        r.status == 200,
                        format!(
                            "status {} name {name:?} interface {url:?} ({skill_count} honest skills, streaming=true)",
                            r.status
                        ),
                    ),
                    Err(reason) => (
                        Outcome::Fail,
                        format!(
                            "status {} name {name:?} interface {url:?}: {reason}",
                            r.status
                        ),
                    ),
                }
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

/// The A2A SSE route (`POST /stream`) dispatches the streaming JSON-RPC
/// methods: an unknown method is `-32601`, and subscribing to a task that
/// does not exist is A2A task-not-found (`-32001`), both under the A2A 1.0
/// name `SubscribeToTask` and the legacy 0.3 alias `tasks/resubscribe`.
/// None of them starts a task.
fn a2a_stream(target: &Target) -> Check {
    let id = format!("a2a.stream@{}", target.port(ports::A2A));
    if let Some(skip) = needs_token(target, &id) {
        return skip;
    }
    timed(id, || {
        let accept = [("Accept", "text/event-stream, application/json")];
        let code = |request: &Value| -> Result<(u16, Option<i64>), String> {
            let r = target.call(ports::A2A, Call::post("/stream", true, &accept, request))?;
            let payload = rpc_payload(&r).unwrap_or(Value::Null);
            Ok((r.status, payload["error"]["code"].as_i64()))
        };
        let unknown =
            json!({"jsonrpc":"2.0","id":8,"method":"susi/conformance-unknown-method","params":{}});
        let missing = |id: i64, method: &str| {
            json!({"jsonrpc":"2.0","id":id,"method":method,
                "params":{"id":"susi-conformance-no-such-task"}})
        };
        match (
            code(&unknown),
            code(&missing(9, "SubscribeToTask")),
            code(&missing(10, "tasks/resubscribe")),
        ) {
            (Ok((s1, c1)), Ok((s2, c2)), Ok((s3, c3))) => verdict(
                c1 == Some(-32601) && c2 == Some(-32001) && c3 == Some(-32001),
                format!(
                    "unknown method -> {s1} code {c1:?} (want -32601); SubscribeToTask missing task -> {s2} code {c2:?}, legacy tasks/resubscribe -> {s3} code {c3:?} (want -32001)"
                ),
            ),
            (Err(e), _, _) | (_, Err(e), _) | (_, _, Err(e)) => (Outcome::Fail, e),
        }
    })
}

/// Public ACP discovery at `GET /.well-known/acp.json` on `9094 + offset`
/// requires no credentials and advertises `/acp` and `/acp/stream`.
fn acp_discover(target: &Target) -> Check {
    timed(
        format!("acp.discover@{}", target.port(ports::A2A)),
        || match target.call(ports::A2A, Call::get("/.well-known/acp.json", false)) {
            Ok(r) => {
                let doc: Value = match serde_json::from_slice(&r.body) {
                    Ok(doc) => doc,
                    Err(e) => {
                        return (
                            Outcome::Fail,
                            format!("status {}: invalid JSON ({e})", r.status),
                        );
                    }
                };
                let schema = doc["schema"].as_str().unwrap_or("");
                let version = doc["protocol_version"].as_str().unwrap_or("");
                let rpc = doc["endpoints"]["rpc"].as_str().unwrap_or("");
                let stream = doc["endpoints"]["stream"].as_str().unwrap_or("");
                let ok = r.status == 200
                    && schema == ACP_DISCOVERY_SCHEMA
                    && !version.is_empty()
                    && rpc == "/acp"
                    && stream == "/acp/stream";
                verdict(
                    ok,
                    format!(
                        "status {} schema {schema:?} protocol_version {version:?} endpoints=({rpc:?}, {stream:?})",
                        r.status
                    ),
                )
            }
            Err(e) => (Outcome::Fail, e),
        },
    )
}

fn acp_post(target: &Target, id: i64, method: &str, params: Value) -> Result<(u16, Value), String> {
    let request = json!({"jsonrpc":"2.0","id":id,"method":method,"params":params});
    let r = target.call(ports::A2A, Call::post("/acp", true, &[], &request))?;
    Ok((r.status, rpc_payload(&r).unwrap_or(Value::Null)))
}

/// Authenticated ACP JSON-RPC 2.0 session lifecycle on `POST /acp`:
/// `initialize` -> `session/new` -> `session/close`, plus `-32601` on an
/// unknown method.
fn acp_rpc(target: &Target) -> Check {
    let id = format!("acp.rpc@{}", target.port(ports::A2A));
    if let Some(skip) = needs_token(target, &id) {
        return skip;
    }
    timed(id, || {
        let (s_init, p_init) = match acp_post(
            target,
            1,
            "initialize",
            json!({"protocolVersion": ACP_PROTOCOL}),
        ) {
            Ok(v) => v,
            Err(e) => return (Outcome::Fail, format!("initialize: {e}")),
        };
        let negotiated = p_init["result"]["protocol_version"]
            .as_str()
            .or_else(|| p_init["result"]["protocolVersion"].as_str())
            .unwrap_or("")
            .to_string();
        if s_init != 200 || negotiated.is_empty() {
            return (
                Outcome::Fail,
                format!("initialize status {s_init}, negotiated {negotiated:?}"),
            );
        }

        let (s_new, p_new) = match acp_post(
            target,
            2,
            "session/new",
            json!({"workspace": ".", "cwd": "."}),
        ) {
            Ok(v) => v,
            Err(e) => return (Outcome::Fail, format!("session/new: {e}")),
        };
        let session_id = p_new["result"]["session_id"]
            .as_str()
            .or_else(|| p_new["result"]["sessionId"].as_str())
            .unwrap_or("")
            .to_string();
        if s_new != 200 || session_id.is_empty() {
            return (
                Outcome::Fail,
                format!("session/new status {s_new}, session_id {session_id:?}"),
            );
        }

        let (s_close, p_close) = match acp_post(
            target,
            3,
            "session/close",
            json!({"session_id": session_id, "sessionId": session_id}),
        ) {
            Ok(v) => v,
            Err(e) => return (Outcome::Fail, format!("session/close: {e}")),
        };
        let closed = p_close["result"]["closed"].as_bool() == Some(true);
        if s_close != 200 || !closed {
            return (
                Outcome::Fail,
                format!("session/close status {s_close}, closed={closed}"),
            );
        }

        let (s_unk, p_unk) = match acp_post(target, 4, "acp/no_such_method", json!({})) {
            Ok(v) => v,
            Err(e) => return (Outcome::Fail, format!("acp/no_such_method: {e}")),
        };
        let code = p_unk["error"]["code"].as_i64();
        verdict(
            s_unk == 200 && code == Some(-32601),
            format!(
                "negotiated {negotiated:?}, session {session_id:?} closed, unknown -> {s_unk} code {code:?} (want -32601)"
            ),
        )
    })
}

/// What one unauthenticated datagram to the discovery port drew back.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum UdpProbe {
    /// No reply within the timeout and no ICMP port-unreachable.
    Silent,
    /// The port answered an unauthenticated datagram (first bytes).
    Answered(String),
    /// ICMP port-unreachable: nothing is bound there.
    Unreachable,
    /// Local socket error.
    Error(String),
}

/// Send `payload` to `host:port` from a connected UDP socket and classify
/// the result. A connected socket surfaces ICMP port-unreachable as
/// `ConnectionReset` (Windows) or `ConnectionRefused` (Unix), which is how
/// silence is told apart from an unbound port.
#[must_use]
pub fn udp_probe(host: &str, port: u16, payload: &[u8], timeout: Duration) -> UdpProbe {
    udp_probe_all(host, port, &[payload], timeout)
        .pop()
        .unwrap_or_else(|| UdpProbe::Error("no probe sent".into()))
}

/// [`udp_probe`] for several payloads at once: one connected socket per
/// payload, every datagram sent first, then each socket read against one
/// shared deadline. A silent port therefore costs one `timeout`, not one per
/// payload. Results are in `payloads` order.
#[must_use]
pub fn udp_probe_all(
    host: &str,
    port: u16,
    payloads: &[&[u8]],
    timeout: Duration,
) -> Vec<UdpProbe> {
    use std::io::ErrorKind;
    use std::net::{ToSocketAddrs, UdpSocket};
    let unreachable = |kind: ErrorKind| {
        matches!(
            kind,
            ErrorKind::ConnectionReset | ErrorKind::ConnectionRefused
        )
    };
    let Some(addr) = (host, port)
        .to_socket_addrs()
        .ok()
        .and_then(|mut a| a.next())
    else {
        let error = UdpProbe::Error(format!("no address for {host}:{port}"));
        return vec![error; payloads.len()];
    };
    let local = if addr.is_ipv4() {
        "0.0.0.0:0"
    } else {
        "[::]:0"
    };
    let sent: Vec<Result<UdpSocket, UdpProbe>> = payloads
        .iter()
        .map(|payload| {
            let socket =
                UdpSocket::bind(local).map_err(|e| UdpProbe::Error(format!("bind: {e}")))?;
            socket
                .connect(addr)
                .map_err(|e| UdpProbe::Error(format!("connect {addr}: {e}")))?;
            match socket.send(payload) {
                Ok(_) => Ok(socket),
                Err(e) if unreachable(e.kind()) => Err(UdpProbe::Unreachable),
                Err(e) => Err(UdpProbe::Error(format!("send: {e}"))),
            }
        })
        .collect();
    let deadline = Instant::now() + timeout;
    let mut buf = [0u8; 4096];
    sent.into_iter()
        .map(|socket| {
            let socket = match socket {
                Ok(socket) => socket,
                Err(probe) => return probe,
            };
            // A zero read timeout is an error; a spent deadline still polls
            // once for a reply or a queued port-unreachable.
            let left = deadline
                .saturating_duration_since(Instant::now())
                .max(Duration::from_millis(1));
            if let Err(e) = socket.set_read_timeout(Some(left)) {
                return UdpProbe::Error(format!("set timeout: {e}"));
            }
            match socket.recv(&mut buf) {
                Ok(n) => UdpProbe::Answered(
                    String::from_utf8_lossy(&buf[..n])
                        .chars()
                        .take(40)
                        .collect(),
                ),
                Err(e) if matches!(e.kind(), ErrorKind::WouldBlock | ErrorKind::TimedOut) => {
                    UdpProbe::Silent
                }
                Err(e) if unreachable(e.kind()) => UdpProbe::Unreachable,
                Err(e) => UdpProbe::Error(format!("recv: {e}")),
            }
        })
        .collect()
}

/// Unauthenticated datagrams the discovery port must never answer: the
/// retired LAN, bare and plaintext-token peer dialects, the retired unbound
/// signed pings (`SUSI_PING_SIG`, `SUSI_PING_SIG2`) and a bound ping
/// (`SUSI_PING_SIG3`) with a forged MAC.
pub const UNAUTHENTICATED_PINGS: [&str; 6] = [
    "SUSI_LAN_PING",
    "SUSI_PING",
    "SUSI_PING:forged-token",
    "SUSI_PING_SIG:susi-conformance:CORE:0:0000000000000000:00:forged",
    "SUSI_PING_SIG2:susi-conformance:CORE:0:0000000000000000:00:127.0.0.1:forged",
    "SUSI_PING_SIG3:susi-conformance:CORE:0:00:3132372e302e302e313a39303933:0:00:forged",
];

/// UDP 9092 is bound and answers only cluster-key signed pings. The suite
/// holds no cluster key, so it checks the other half of that contract: every
/// unauthenticated datagram draws silence, and the port is not unbound.
/// Silence across a network cannot prove the port is bound (a firewall can
/// drop the ICMP reply), so a non-loopback host gets a warning, not a pass.
fn udp_discovery(target: &Target) -> Check {
    let port = target.port(ports::UDP_DISCOVERY);
    timed(format!("udp.discovery@{port}"), || {
        // One shared silence window for all pings (see `udp_probe_all`), so
        // the check fits a tight `--max-ms` budget even with a long timeout.
        let wait = target.timeout.min(Duration::from_millis(500));
        let payloads = UNAUTHENTICATED_PINGS.map(str::as_bytes);
        let probes = udp_probe_all(&target.host, port, &payloads, wait);
        for (ping, probe) in UNAUTHENTICATED_PINGS.iter().zip(probes) {
            match probe {
                UdpProbe::Silent => {}
                UdpProbe::Answered(reply) => {
                    return (
                        Outcome::Fail,
                        format!("unauthenticated {ping:?} was answered: {reply:?}"),
                    )
                }
                UdpProbe::Unreachable => {
                    return (
                        Outcome::Fail,
                        format!("nothing bound on udp {port} (ICMP port unreachable)"),
                    )
                }
                UdpProbe::Error(e) => return (Outcome::Fail, e),
            }
        }
        let loopback = target.host == "localhost"
            || target
                .host
                .parse::<std::net::IpAddr>()
                .is_ok_and(|ip| ip.is_loopback());
        let detail = format!(
            "{} unauthenticated pings drew no reply; signed pings only",
            UNAUTHENTICATED_PINGS.len()
        );
        if loopback {
            (Outcome::Pass, detail)
        } else {
            (
                Outcome::Warn,
                format!("{detail} (remote host: silence cannot prove the port is bound)"),
            )
        }
    })
}

/// Opt-in positive UDP discovery check: when a cluster-key signed
/// `SUSI_PING_SIG3` datagram is supplied, UDP 9092 must answer with a
/// `SUSI_PONG_SIG*` frame. A bound ping verifies only from the source
/// address it names, within 60 seconds of its timestamp and once per nonce,
/// so the datagram must be freshly minted on the host running the suite.
fn udp_discovery_signed(target: &Target, optional: &Optional) -> Check {
    let port = target.port(ports::UDP_DISCOVERY);
    let id = format!("udp.discovery-signed@{port}");
    let Some(ref ping) = optional.signed_udp_ping else {
        return Check {
            id,
            outcome: Outcome::Skip,
            millis: 0,
            detail: "opt-in: signed_udp_ping verifies positive SUSI_PONG_SIG* reply".into(),
        };
    };
    timed(id, || {
        let wait = target.timeout.min(Duration::from_millis(500));
        match udp_probe(&target.host, port, ping.as_bytes(), wait) {
            UdpProbe::Answered(reply)
                if reply.starts_with("SUSI_PONG_SIG:") || reply.starts_with("SUSI_PONG_SIG2:") =>
            {
                if let Some(ref prefix) = optional.signed_udp_pong_prefix {
                    if !reply.starts_with(prefix) {
                        return (
                            Outcome::Fail,
                            format!(
                                "signed pong {reply:?} did not match required prefix {prefix:?}"
                            ),
                        );
                    }
                }
                (Outcome::Pass, format!("signed ping answered: {reply:?}"))
            }
            UdpProbe::Answered(reply) => (
                Outcome::Fail,
                format!("signed ping drew unexpected reply: {reply:?}"),
            ),
            UdpProbe::Silent => (
                Outcome::Fail,
                "signed ping drew no reply within timeout".into(),
            ),
            UdpProbe::Unreachable => (
                Outcome::Fail,
                format!("nothing bound on udp {port} (ICMP port unreachable)"),
            ),
            UdpProbe::Error(e) => (Outcome::Fail, e),
        }
    })
}

/// Opt-in GEMI SSE check: `stream: true` on `/v1/chat/completions` answers
/// `text/event-stream` with `data:` chunks and a final `data: [DONE]`. The
/// request is local-only and 1 token. A host with no local inference target
/// answers `503` "request prohibits cloud inference ..." instead, which is a
/// warning: SSE was not exercised. Any other non-SSE reply fails.
fn gemi_stream(target: &Target, optional: &Optional) -> Check {
    let id = format!("gemi.stream@{}", target.port(ports::GEMI));
    if !optional.inference {
        return Check {
            id,
            outcome: Outcome::Skip,
            millis: 0,
            detail: "opt-in: --inference runs one local, 1-token streamed completion".into(),
        };
    }
    if let Some(skip) = needs_token(target, &id) {
        return skip;
    }
    timed(id, || {
        let request = json!({"stream": true, "max_tokens": 1,
            "messages": [{"role": "user", "content": "Reply with one word."}],
            "susi": {"allow_cloud": false}});
        let accept = [("Accept", "text/event-stream")];
        let r = match target.call(
            ports::GEMI,
            Call::post("/v1/chat/completions", true, &accept, &request),
        ) {
            Ok(r) => r,
            Err(e) => return (Outcome::Fail, e),
        };
        let content_type = r.header("content-type").unwrap_or("").to_string();
        if r.status == 200 && content_type.starts_with("text/event-stream") {
            let text = r.text();
            let chunks = text
                .lines()
                .filter_map(|l| l.strip_prefix("data:"))
                .map(str::trim)
                .filter(|d| serde_json::from_str::<Value>(d).is_ok())
                .count();
            let done = text.lines().any(|l| l.trim() == "data: [DONE]");
            return verdict(
                chunks > 0 && done,
                format!("SSE: {chunks} JSON chunks, [DONE] {done}"),
            );
        }
        gemi_stream_fallback(r.status, &content_type, &r.body)
    })
}

/// Classify a non-SSE reply to the streamed completion. Only the documented
/// "no local inference target" answer (`503`, placement reason starting
/// `request prohibits cloud inference`) is a warning; every other error is a
/// streaming regression and fails.
fn gemi_stream_fallback(status: u16, content_type: &str, body: &[u8]) -> (Outcome, String) {
    let message = serde_json::from_slice::<Value>(body)
        .ok()
        .and_then(|v| v["error"]["message"].as_str().map(str::to_string));
    match message {
        Some(m) if status == 503 && m.starts_with("request prohibits cloud inference") => (
            Outcome::Warn,
            format!("status {status}: {m} (no local inference target; SSE not exercised)"),
        ),
        Some(m) => (
            Outcome::Fail,
            format!("status {status} content-type {content_type:?}: {m}"),
        ),
        None => (
            Outcome::Fail,
            format!("status {status} content-type {content_type:?}"),
        ),
    }
}

/// Read newline-delimited JSON-RPC from `lines` until the response with
/// `id` arrives or `timeout` passes. MCP stdio reserves stdout for MCP
/// messages, so any non-empty line that is not a JSON-RPC 2.0 object (a
/// banner, a log line) is an error, reported with its first 80 chars.
fn stdio_response(
    lines: &std::sync::mpsc::Receiver<String>,
    id: i64,
    timeout: Duration,
) -> Result<Value, String> {
    let deadline = Instant::now() + timeout;
    loop {
        let left = deadline.saturating_duration_since(Instant::now());
        let line = lines
            .recv_timeout(left)
            .map_err(|_| format!("no response to request {id} on stdout"))?;
        let line = line.trim();
        if line.is_empty() {
            continue;
        }
        match serde_json::from_str::<Value>(line) {
            Ok(value) if value.get("jsonrpc").and_then(Value::as_str) == Some("2.0") => {
                if value["id"].as_i64() == Some(id) {
                    return Ok(value);
                }
            }
            _ => {
                let head: String = line.chars().take(80).collect();
                return Err(format!(
                    "stdout carried a non-MCP line (stdio must carry only JSON-RPC): {head:?}"
                ));
            }
        }
    }
}

/// One MCP stdio session: `initialize`, `notifications/initialized`,
/// `tools/list` (every page). Returns `(negotiated revision, server name,
/// tool count)`.
fn stdio_session(
    stdin: &mut impl Write,
    lines: &std::sync::mpsc::Receiver<String>,
    first: Duration,
    timeout: Duration,
) -> Result<(String, String, usize), String> {
    let mut send = |value: Value| {
        writeln!(stdin, "{value}")
            .and_then(|()| stdin.flush())
            .map_err(|e| format!("write to stdin: {e}"))
    };
    send(
        json!({"jsonrpc":"2.0","id":1,"method":"initialize","params":{
        "protocolVersion": MCP_SESSION, "capabilities": {},
        "clientInfo": {"name":"susi-conformance","version": env!("CARGO_PKG_VERSION")}}}),
    )?;
    let init = stdio_response(lines, 1, first)?;
    let negotiated = init["result"]["protocolVersion"]
        .as_str()
        .unwrap_or("")
        .to_string();
    let server = init["result"]["serverInfo"]["name"]
        .as_str()
        .unwrap_or("")
        .to_string();
    send(json!({"jsonrpc":"2.0","method":"notifications/initialized"}))?;
    let tools = list_all_tools(|page, cursor| {
        let id = page_id(2, page);
        send(json!({"jsonrpc":"2.0","id":id,"method":"tools/list","params":list_params(cursor)}))?;
        stdio_response(lines, id, timeout)
    })?;
    Ok((negotiated, server, tools.len()))
}

/// Spawn `<bin> <args...>` with piped stdin/stdout, run `session`, then close
/// stdin and reap the child regardless of outcome.
fn run_stdio_child<T>(
    bin: &std::path::Path,
    args: &[&str],
    session: impl FnOnce(
        &mut std::process::ChildStdin,
        &std::sync::mpsc::Receiver<String>,
    ) -> Result<T, String>,
) -> Result<T, String> {
    let mut child = Command::new(bin)
        .args(args)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()
        .map_err(|e| format!("spawn {}: {e}", bin.display()))?;
    let (Some(mut stdin), Some(stdout)) = (child.stdin.take(), child.stdout.take()) else {
        let _ = child.kill();
        let _ = child.wait();
        return Err("child pipes unavailable".into());
    };
    let (tx, rx) = std::sync::mpsc::channel();
    std::thread::spawn(move || {
        for line in BufReader::new(stdout).lines().map_while(Result::ok) {
            if tx.send(line).is_err() {
                break;
            }
        }
    });
    let outcome = session(&mut stdin, &rx);
    drop(stdin);
    let deadline = Instant::now() + Duration::from_secs(2);
    while Instant::now() < deadline && matches!(child.try_wait(), Ok(None)) {
        std::thread::sleep(Duration::from_millis(50));
    }
    if matches!(child.try_wait(), Ok(None)) {
        let _ = child.kill();
    }
    let _ = child.wait();
    outcome
}

/// Opt-in MCP stdio check: `<susi_bin> mcp serve` speaks newline-delimited
/// JSON-RPC at the session revision. The child is stopped before the check
/// returns, whatever the outcome.
fn mcp_stdio(target: &Target, optional: &Optional) -> Check {
    let id = "mcp.stdio".to_string();
    let Some(bin) = optional.susi_bin.as_ref() else {
        return Check {
            id,
            outcome: Outcome::Skip,
            millis: 0,
            detail: "opt-in: --susi-bin PATH runs `PATH mcp serve`".into(),
        };
    };
    timed(id, || {
        // Process start can include substrate boot; the first reply gets
        // at least 15 s.
        let first = target.timeout.max(Duration::from_secs(15));
        match run_stdio_child(bin, &["mcp", "serve"], |stdin, rx| {
            stdio_session(stdin, rx, first, target.timeout)
        }) {
            Ok((negotiated, server, tools)) => verdict(
                !negotiated.is_empty() && !server.is_empty(),
                format!("server {server:?} negotiated {negotiated:?}, {tools} tools"),
            ),
            Err(e) => (Outcome::Fail, e),
        }
    })
}

/// One ACP stdio session: `initialize`, `session/new`, `session/list`, and
/// `session/close`. Rejects any non-JSON-RPC banner line on stdout via
/// [`stdio_response`].
fn acp_stdio_session(
    stdin: &mut impl Write,
    lines: &std::sync::mpsc::Receiver<String>,
    first: Duration,
    timeout: Duration,
) -> Result<(String, String), String> {
    let mut send = |value: Value| {
        writeln!(stdin, "{value}")
            .and_then(|()| stdin.flush())
            .map_err(|e| format!("write to stdin: {e}"))
    };
    send(json!({
        "jsonrpc": "2.0",
        "id": 1,
        "method": "initialize",
        "params": {"protocolVersion": ACP_PROTOCOL}
    }))?;
    let init = stdio_response(lines, 1, first)?;
    if let Some(err) = init.get("error") {
        return Err(format!("initialize JSON-RPC error: {err}"));
    }
    let negotiated = init["result"]["protocol_version"]
        .as_str()
        .or_else(|| init["result"]["protocolVersion"].as_str())
        .unwrap_or("")
        .to_string();
    if negotiated.is_empty() {
        return Err("initialize returned empty protocol_version".into());
    }

    send(json!({
        "jsonrpc": "2.0",
        "id": 2,
        "method": "session/new",
        "params": {"workspace": ".", "cwd": "."}
    }))?;
    let created = stdio_response(lines, 2, timeout)?;
    if let Some(err) = created.get("error") {
        return Err(format!("session/new JSON-RPC error: {err}"));
    }
    let session_id = created["result"]["session_id"]
        .as_str()
        .or_else(|| created["result"]["sessionId"].as_str())
        .unwrap_or("")
        .to_string();
    if session_id.is_empty() {
        return Err("session/new returned empty session_id".into());
    }

    send(json!({
        "jsonrpc": "2.0",
        "id": 3,
        "method": "session/list",
        "params": {}
    }))?;
    let listed = stdio_response(lines, 3, timeout)?;
    if let Some(err) = listed.get("error") {
        return Err(format!("session/list JSON-RPC error: {err}"));
    }
    let Some(sessions) = listed["result"]["sessions"].as_array() else {
        return Err("session/list returned no `sessions` array".into());
    };
    let listed_match = sessions.iter().any(|s| {
        s["session_id"].as_str().or_else(|| s["sessionId"].as_str()) == Some(session_id.as_str())
    });
    if !listed_match {
        return Err(format!(
            "session/list did not include created session {session_id:?}"
        ));
    }

    send(json!({
        "jsonrpc": "2.0",
        "id": 4,
        "method": "session/close",
        "params": {"session_id": session_id, "sessionId": session_id}
    }))?;
    let closed = stdio_response(lines, 4, timeout)?;
    if let Some(err) = closed.get("error") {
        return Err(format!("session/close JSON-RPC error: {err}"));
    }
    if closed["result"]["closed"].as_bool() != Some(true) {
        return Err("session/close did not confirm `closed: true`".into());
    }
    Ok((negotiated, session_id))
}

/// Opt-in ACP stdio check: `<susi_bin> acp serve` speaks pure newline-delimited
/// JSON-RPC 2.0 on stdout with zero banner lines, negotiates ACP, and completes
/// `session/new` -> `session/list` -> `session/close`.
fn acp_stdio(target: &Target, optional: &Optional) -> Check {
    let id = "acp.stdio".to_string();
    let Some(bin) = optional.susi_bin.as_ref() else {
        return Check {
            id,
            outcome: Outcome::Skip,
            millis: 0,
            detail: "opt-in: --susi-bin PATH runs `PATH acp serve`".into(),
        };
    };
    timed(id, || {
        let first = target.timeout.max(Duration::from_secs(15));
        match run_stdio_child(bin, &["acp", "serve"], |stdin, rx| {
            acp_stdio_session(stdin, rx, first, target.timeout)
        }) {
            Ok((negotiated, session_id)) => verdict(
                !negotiated.is_empty() && !session_id.is_empty(),
                format!("negotiated {negotiated:?}, session {session_id:?} listed and closed"),
            ),
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

/// Run every contract check against `target`, in a stable order. The
/// opt-in checks (GEMI SSE, MCP stdio, ACP stdio) report `skip`; see [`run_with`].
#[must_use]
pub fn run(target: &Target) -> Vec<Check> {
    run_with(target, &Optional::default())
}

/// [`run`] plus the opt-in checks `optional` enables.
#[must_use]
pub fn run_with(target: &Target, optional: &Optional) -> Vec<Check> {
    let mut checks = Vec::new();
    for base in [ports::MCP, ports::GEMI, ports::MCP_ALIAS, ports::A2A] {
        checks.push(health(target, base));
    }
    checks.push(auth_required(target, ports::MCP, "POST", "/mcp"));
    checks.push(auth_required(target, ports::GEMI, "GET", "/v1/models"));
    checks.push(auth_required(target, ports::MCP_ALIAS, "POST", "/mcp"));
    checks.push(auth_required(target, ports::A2A, "POST", "/"));
    checks.push(auth_required_as(
        target,
        ports::A2A,
        "POST",
        "/stream",
        "/stream",
    ));
    checks.push(auth_required_as(target, ports::A2A, "POST", "/acp", "/acp"));
    checks.push(auth_required_as(
        target,
        ports::A2A,
        "POST",
        "/acp/stream",
        "/acp/stream",
    ));
    checks.push(mcp_discover(target, ports::MCP));
    checks.push(mcp_discover(target, ports::MCP_ALIAS));
    checks.push(mcp_tools_at(target, ports::MCP));
    checks.push(mcp_tools_at(target, ports::MCP_ALIAS));
    checks.push(mcp_unknown(target));
    checks.push(mcp_session_at(target, ports::MCP));
    checks.push(mcp_session_at(target, ports::MCP_ALIAS));
    checks.push(a2a_card(target));
    checks.push(a2a_unknown(target));
    checks.push(a2a_stream(target));
    checks.push(acp_discover(target));
    checks.push(acp_rpc(target));
    checks.push(udp_discovery(target));
    checks.push(udp_discovery_signed(target, optional));
    checks.push(gemi_models(target));
    checks.push(gemi_stream(target, optional));
    checks.push(mcp_stdio(target, optional));
    checks.push(acp_stdio(target, optional));
    checks
}

/// Fail every passing check slower than `max_millis`: the speed mandate made
/// enforceable. Results that already failed, warned or skipped are unchanged.
pub fn enforce_latency_budget(checks: &mut [Check], max_millis: u128) {
    for check in checks.iter_mut() {
        if check.outcome == Outcome::Pass && check.millis > max_millis {
            check.outcome = Outcome::Fail;
            check.detail = format!(
                "slow: {} ms exceeds the {max_millis} ms budget ({})",
                check.millis, check.detail
            );
        }
    }
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
    fn list_all_tools_follows_next_cursor_until_absent() {
        let pages = [
            json!({"jsonrpc":"2.0","id":2,"result":{"tools":[{"name":"a"}],"nextCursor":"r:1"}}),
            json!({"jsonrpc":"2.0","id":3,"result":{"tools":[{"name":"b"}],"nextCursor":"r:2"}}),
            json!({"jsonrpc":"2.0","id":4,"result":{"tools":[{"name":"c"}]}}),
        ];
        let mut seen = Vec::new();
        let tools = list_all_tools(|page, cursor| {
            seen.push((page_id(2, page), list_params(cursor)));
            Ok(pages[page].clone())
        })
        .unwrap();
        let names: Vec<&str> = tools.iter().filter_map(|t| t["name"].as_str()).collect();
        assert_eq!(names, ["a", "b", "c"]);
        assert_eq!(
            seen,
            [
                (2, json!({})),
                (3, json!({"cursor":"r:1"})),
                (4, json!({"cursor":"r:2"}))
            ]
        );
    }

    #[test]
    fn list_all_tools_fails_clearly_on_cursor_error_shape_and_runaway() {
        let stale = list_all_tools(|page, _| {
            Ok(if page == 0 {
                json!({"result":{"tools":[],"nextCursor":"r:1"}})
            } else {
                json!({"error":{"code":-32602,"message":"Catalog changed"}})
            })
        })
        .unwrap_err();
        assert!(
            stale.contains("page 2") && stale.contains("Catalog changed"),
            "{stale}"
        );
        let shapeless = list_all_tools(|_, _| Ok(json!({"result":{}}))).unwrap_err();
        assert!(shapeless.contains("no tools array"), "{shapeless}");
        let numeric =
            list_all_tools(|_, _| Ok(json!({"result":{"tools":[],"nextCursor":7}}))).unwrap_err();
        assert!(numeric.contains("non-string nextCursor"), "{numeric}");
        let mut calls = 0;
        let runaway = list_all_tools(|page, _| {
            calls += 1;
            Ok(json!({"result":{"tools":[],"nextCursor":format!("r:{page}")}}))
        })
        .unwrap_err();
        assert!(runaway.contains("still paging"), "{runaway}");
        assert_eq!(calls, MAX_LIST_PAGES);
    }

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
    fn latency_budget_fails_only_slow_passes() {
        let check = |outcome, millis| Check {
            id: "x".into(),
            outcome,
            millis,
            detail: "d".into(),
        };
        let mut checks = vec![
            check(Outcome::Pass, 5),
            check(Outcome::Pass, 500),
            check(Outcome::Warn, 900),
            check(Outcome::Skip, 0),
        ];
        enforce_latency_budget(&mut checks, 100);
        let outcomes: Vec<_> = checks.iter().map(|c| c.outcome).collect();
        assert_eq!(
            outcomes,
            [Outcome::Pass, Outcome::Fail, Outcome::Warn, Outcome::Skip]
        );
        assert!(checks[1]
            .detail
            .starts_with("slow: 500 ms exceeds the 100 ms budget"));
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
    fn udp_probe_tells_silence_from_answers_and_unbound_ports() {
        use std::net::UdpSocket;
        let wait = Duration::from_millis(300);
        // Bound and silent: the contract for unauthenticated pings.
        let silent = UdpSocket::bind("127.0.0.1:0").unwrap();
        let port = silent.local_addr().unwrap().port();
        assert_eq!(
            udp_probe("127.0.0.1", port, b"SUSI_PING", wait),
            UdpProbe::Silent
        );
        // A responder that answers anything violates it.
        let echo = UdpSocket::bind("127.0.0.1:0").unwrap();
        let echo_port = echo.local_addr().unwrap().port();
        std::thread::spawn(move || {
            let mut buf = [0u8; 64];
            if let Ok((n, src)) = echo.recv_from(&mut buf) {
                let _ = echo.send_to(&buf[..n], src);
            }
        });
        assert_eq!(
            udp_probe("127.0.0.1", echo_port, b"SUSI_LAN_PING", wait),
            UdpProbe::Answered("SUSI_LAN_PING".into())
        );
        // Nothing bound: ICMP port-unreachable, never mistaken for silence.
        let gone = UdpSocket::bind("127.0.0.1:0").unwrap();
        let gone_port = gone.local_addr().unwrap().port();
        drop(gone);
        assert_eq!(
            udp_probe("127.0.0.1", gone_port, b"SUSI_PING", wait),
            UdpProbe::Unreachable
        );
        drop(silent);
    }

    #[test]
    fn opt_in_checks_skip_unless_enabled() {
        let checks = run(&Target {
            host: "127.0.0.1".into(),
            port_offset: 50_000,
            token: None,
            timeout: Duration::from_millis(200),
        });
        for id in [
            "udp.discovery-signed@59092",
            "gemi.stream@59091",
            "mcp.stdio",
            "acp.stdio",
        ] {
            let check = checks.iter().find(|c| c.id == id).unwrap();
            assert_eq!(check.outcome, Outcome::Skip, "{id}");
            assert!(check.detail.starts_with("opt-in"), "{id}");
        }
    }

    #[test]
    fn udp_discovery_signed_passes_on_valid_pong_and_fails_on_silence_or_bad_reply() {
        use std::net::UdpSocket;
        assert_eq!(UNAUTHENTICATED_PINGS.len(), 6);
        assert!(UNAUTHENTICATED_PINGS
            .iter()
            .any(|p| p.starts_with("SUSI_PING_SIG3:") && p.ends_with(":forged")));
        assert!(UNAUTHENTICATED_PINGS.contains(&"SUSI_PING:forged-token"));
        assert!(UNAUTHENTICATED_PINGS.contains(
            &"SUSI_PING_SIG2:susi-conformance:CORE:0:0000000000000000:00:127.0.0.1:forged"
        ));

        let opt = Optional {
            signed_udp_ping: Some(
                "SUSI_PING_SIG2:susi-conformance:CORE:9090:1:deadbeef:127.0.0.1:cafebabe".into(),
            ),
            ..Optional::default()
        };

        // 1. Valid SUSI_PONG_SIG2 reply passes.
        let valid = UdpSocket::bind("127.0.0.1:0").unwrap();
        let valid_port = valid.local_addr().unwrap().port();
        assert!(valid_port >= ports::UDP_DISCOVERY);
        std::thread::spawn(move || {
            let mut buf = [0u8; 256];
            if let Ok((_n, src)) = valid.recv_from(&mut buf) {
                let _ = valid.send_to(
                    b"SUSI_PONG_SIG2:host:CORE:9090:1:deadbeef:127.0.0.1:cafebabe",
                    src,
                );
            }
        });
        let check_ok = udp_discovery_signed(
            &Target {
                host: "127.0.0.1".into(),
                port_offset: valid_port - ports::UDP_DISCOVERY,
                token: None,
                timeout: Duration::from_millis(200),
            },
            &opt,
        );
        assert_eq!(check_ok.outcome, Outcome::Pass, "{}", check_ok.detail);

        // 2. Unauthenticated/legacy pong reply fails.
        let bad = UdpSocket::bind("127.0.0.1:0").unwrap();
        let bad_port = bad.local_addr().unwrap().port();
        assert!(bad_port >= ports::UDP_DISCOVERY);
        std::thread::spawn(move || {
            let mut buf = [0u8; 256];
            if let Ok((_n, src)) = bad.recv_from(&mut buf) {
                let _ = bad.send_to(b"SUSI_PONG:legacy", src);
            }
        });
        let check_bad = udp_discovery_signed(
            &Target {
                host: "127.0.0.1".into(),
                port_offset: bad_port - ports::UDP_DISCOVERY,
                token: None,
                timeout: Duration::from_millis(200),
            },
            &opt,
        );
        assert_eq!(check_bad.outcome, Outcome::Fail, "{}", check_bad.detail);

        // 3. Silent socket fails.
        let silent = UdpSocket::bind("127.0.0.1:0").unwrap();
        let silent_port = silent.local_addr().unwrap().port();
        assert!(silent_port >= ports::UDP_DISCOVERY);
        let check_silent = udp_discovery_signed(
            &Target {
                host: "127.0.0.1".into(),
                port_offset: silent_port - ports::UDP_DISCOVERY,
                token: None,
                timeout: Duration::from_millis(100),
            },
            &opt,
        );
        assert_eq!(
            check_silent.outcome,
            Outcome::Fail,
            "{}",
            check_silent.detail
        );
        drop(silent);
    }

    /// A UDP port known to be free: bound, then released. Ports at a fixed
    /// offset can sit in the OS ephemeral range and be held by anything.
    fn free_udp_port() -> u16 {
        let socket = std::net::UdpSocket::bind("127.0.0.1:0").unwrap();
        socket.local_addr().unwrap().port()
    }

    #[test]
    fn udp_discovery_fails_when_nothing_is_bound() {
        let port = free_udp_port();
        assert!(port >= ports::UDP_DISCOVERY, "ephemeral port {port}");
        let check = udp_discovery(&Target {
            host: "127.0.0.1".into(),
            port_offset: port - ports::UDP_DISCOVERY,
            token: None,
            timeout: Duration::from_millis(200),
        });
        assert_eq!(check.outcome, Outcome::Fail, "{}", check.detail);
    }

    /// A correct daemon stays silent, so the check must cost one silence
    /// window, not one per ping: CI runs `--timeout-ms 30000 --max-ms 2000`.
    #[test]
    fn udp_discovery_fits_the_ci_latency_budget() {
        let silent = std::net::UdpSocket::bind("127.0.0.1:0").unwrap();
        let port = silent.local_addr().unwrap().port();
        assert!(port >= ports::UDP_DISCOVERY, "ephemeral port {port}");
        let mut checks = vec![udp_discovery(&Target {
            host: "127.0.0.1".into(),
            port_offset: port - ports::UDP_DISCOVERY,
            token: None,
            timeout: Duration::from_secs(30),
        })];
        enforce_latency_budget(&mut checks, 2_000);
        assert_eq!(checks[0].outcome, Outcome::Pass, "{}", checks[0].detail);
        assert!(checks[0].millis < 2_000, "{} ms", checks[0].millis);
        drop(silent);
    }

    #[test]
    fn check_ids_are_unique() {
        let checks = run_with(
            &Target {
                host: "127.0.0.1".into(),
                port_offset: 50_000,
                token: Some("unused".into()),
                timeout: Duration::from_millis(200),
            },
            &Optional::default(),
        );
        let mut ids: Vec<&str> = checks.iter().map(|c| c.id.as_str()).collect();
        for expected in [
            "auth.required@59094/stream",
            "auth.required@59094/acp",
            "auth.required@59094/acp/stream",
            "acp.discover@59094",
            "acp.rpc@59094",
            "acp.stdio",
        ] {
            assert!(ids.contains(&expected), "missing {expected} in {ids:?}");
        }
        ids.sort_unstable();
        let before = ids.len();
        ids.dedup();
        assert_eq!(ids.len(), before, "duplicate check ids");
    }

    #[test]
    fn gemi_stream_warns_only_without_a_local_target() {
        let error = |m: &str| json!({"error": {"message": m}}).to_string().into_bytes();
        let (outcome, _) = gemi_stream_fallback(
            503,
            "application/json",
            &error("request prohibits cloud inference and no ready local model is available"),
        );
        assert_eq!(outcome, Outcome::Warn);
        for (status, message) in [
            (500, "stream failed"),
            (400, "stream not supported"),
            (503, "overloaded"),
        ] {
            let (outcome, detail) =
                gemi_stream_fallback(status, "application/json", &error(message));
            assert_eq!(outcome, Outcome::Fail, "{status}: {detail}");
        }
        assert_eq!(
            gemi_stream_fallback(200, "text/plain", b"hi").0,
            Outcome::Fail
        );
    }

    #[test]
    fn stdio_rejects_non_mcp_stdout_lines() {
        let reply = json!({"jsonrpc":"2.0","id":1,"result":{}}).to_string();
        let (tx, rx) = std::sync::mpsc::channel();
        tx.send(String::new()).unwrap();
        tx.send(json!({"jsonrpc":"2.0","method":"notifications/message"}).to_string())
            .unwrap();
        tx.send(reply.clone()).unwrap();
        assert!(stdio_response(&rx, 1, Duration::from_millis(200)).is_ok());
        let (tx, rx) = std::sync::mpsc::channel();
        tx.send("[SUSI] bootloader: kernel loaded".to_string())
            .unwrap();
        tx.send(reply).unwrap();
        let err = stdio_response(&rx, 1, Duration::from_millis(200)).unwrap_err();
        assert!(err.contains("bootloader"), "{err}");
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
        // UDP silence on a port in the ephemeral range is not proof of
        // absence (any process may hold it); the TCP checks are.
        assert!(checks
            .iter()
            .filter(|c| !c.id.starts_with("udp."))
            .all(|c| c.outcome != Outcome::Pass));
    }

    #[test]
    fn contract_and_out_of_contract_port_inventories_are_disjoint_and_complete() {
        assert_eq!(CONTRACT_PORTS, [9090, 9091, 9092, 9093, 9094]);
        for port in CONTRACT_PORTS {
            assert!(is_contract_port(port));
            assert_eq!(out_of_contract_role(port), None);
        }
        let expected_internal = [
            9095, 9180, 9181, 18080, 18081, 18082, 18083, 18084, 19090, 19091, 19092, 19093,
        ];
        assert_eq!(OUT_OF_CONTRACT_PORTS.len(), expected_internal.len());
        for port in expected_internal {
            assert!(
                !is_contract_port(port),
                "internal port {port} must be out of contract"
            );
            assert!(
                out_of_contract_role(port).is_some_and(|role| !role.is_empty()),
                "missing out_of_contract_role for {port}"
            );
        }
        assert!(OUT_OF_CONTRACT_IPC.contains(&"plane_bus"));
    }

    #[test]
    fn mcp_alias_9093_runs_tools_list_conformance_check() {
        let checks = run_with(
            &Target {
                host: "127.0.0.1".into(),
                port_offset: 50_000,
                token: None,
                timeout: Duration::from_millis(100),
            },
            &Optional::default(),
        );
        let ids: Vec<&str> = checks.iter().map(|c| c.id.as_str()).collect();
        assert!(ids.contains(&"mcp.tools-list@59090"), "{ids:?}");
        assert!(ids.contains(&"mcp.tools-list@59093"), "{ids:?}");
    }

    #[test]
    fn mcp_alias_9093_runs_stateful_session_conformance_check() {
        let checks = run_with(
            &Target {
                host: "127.0.0.1".into(),
                port_offset: 50_000,
                token: None,
                timeout: Duration::from_millis(100),
            },
            &Optional::default(),
        );
        let ids: Vec<&str> = checks.iter().map(|c| c.id.as_str()).collect();
        assert!(ids.contains(&"mcp.session-2025-06-18@59090"), "{ids:?}");
        assert!(ids.contains(&"mcp.session-2025-06-18@59093"), "{ids:?}");
    }

    #[test]
    fn agent_card_honesty_validator_accepts_per_capability_skills_and_rejects_dishonest_cards() {
        let valid_card = json!({
            "name": "susi-gawd",
            "supportedInterfaces": [{"url": "http://127.0.0.1:9094/"}],
            "capabilities": {"streaming": true},
            "skills": [
                {"id": "missions", "name": "missions", "description": "Run a GAWD mission."},
                {"id": "agents", "name": "agents", "description": "Delegate to installed agents."},
                {"id": "frameworks", "name": "frameworks", "description": "Run external frameworks."},
                {"id": "models", "name": "models", "description": "Route through GEMI models."}
            ]
        });
        assert_eq!(validate_agent_card_honesty(&valid_card), Ok(4));

        // Missing per-capability skill ("models")
        let missing_skill_card = json!({
            "name": "susi-gawd",
            "supportedInterfaces": [{"url": "http://127.0.0.1:9094/"}],
            "capabilities": {"streaming": true},
            "skills": [
                {"id": "missions", "name": "missions", "description": "Run a GAWD mission."},
                {"id": "agents", "name": "agents", "description": "Delegate to installed agents."},
                {"id": "frameworks", "name": "frameworks", "description": "Run external frameworks."}
            ]
        });
        let err_missing = validate_agent_card_honesty(&missing_skill_card).unwrap_err();
        assert!(err_missing.contains("models"), "{err_missing}");

        // streaming: false
        let no_streaming_card = json!({
            "name": "susi-gawd",
            "supportedInterfaces": [{"url": "http://127.0.0.1:9094/"}],
            "capabilities": {"streaming": false},
            "skills": valid_card["skills"].clone()
        });
        let err_streaming = validate_agent_card_honesty(&no_streaming_card).unwrap_err();
        assert!(err_streaming.contains("streaming"), "{err_streaming}");

        // Dishonest card claiming egui / desktop ui
        let dishonest_card = json!({
            "name": "susi-gawd",
            "supportedInterfaces": [{"url": "http://127.0.0.1:9094/"}],
            "capabilities": {"streaming": true},
            "skills": [
                {"id": "missions", "name": "missions", "description": "Renders an egui desktop UI dashboard."},
                {"id": "agents", "name": "agents", "description": "Delegate to installed agents."},
                {"id": "frameworks", "name": "frameworks", "description": "Run external frameworks."},
                {"id": "models", "name": "models", "description": "Route through GEMI models."}
            ]
        });
        let err_dishonest = validate_agent_card_honesty(&dishonest_card).unwrap_err();
        assert!(err_dishonest.contains("dishonest skill"), "{err_dishonest}");
    }

    struct MockReply {
        status: u16,
        content_type: &'static str,
        headers: Vec<(&'static str, &'static str)>,
        body: String,
    }

    impl MockReply {
        fn json(status: u16, value: &Value) -> Self {
            Self {
                status,
                content_type: "application/json",
                headers: Vec::new(),
                body: value.to_string(),
            }
        }
    }

    fn read_http_request(stream: &mut std::net::TcpStream) -> String {
        use std::io::Read;
        let mut buf = Vec::new();
        let mut chunk = [0u8; 1024];
        let mut header_end = None;
        let mut content_len = 0usize;
        while let Ok(n) = stream.read(&mut chunk) {
            if n == 0 {
                break;
            }
            buf.extend_from_slice(&chunk[..n]);
            if header_end.is_none() {
                if let Some(pos) = buf.windows(4).position(|w| w == b"\r\n\r\n") {
                    header_end = Some(pos + 4);
                    let head = String::from_utf8_lossy(&buf[..pos]);
                    for line in head.lines() {
                        if let Some((k, v)) = line.split_once(':') {
                            if k.trim().eq_ignore_ascii_case("content-length") {
                                content_len = v.trim().parse().unwrap_or(0);
                            }
                        }
                    }
                }
            }
            if let Some(end) = header_end {
                if buf.len() >= end + content_len {
                    break;
                }
            }
        }
        String::from_utf8_lossy(&buf).into_owned()
    }

    fn spawn_loopback_http(
        base_port: u16,
        replies: Vec<MockReply>,
    ) -> (Target, std::thread::JoinHandle<Vec<String>>) {
        use std::io::Write;
        let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let bound_port = listener.local_addr().unwrap().port();
        assert!(bound_port >= base_port, "ephemeral port {bound_port}");
        let handle = std::thread::spawn(move || {
            let mut requests = Vec::with_capacity(replies.len());
            for reply in replies {
                let (mut stream, _) = listener.accept().unwrap();
                let _ = stream.set_read_timeout(Some(Duration::from_secs(2)));
                let _ = stream.set_write_timeout(Some(Duration::from_secs(2)));
                requests.push(read_http_request(&mut stream));
                let mut raw = format!(
                    "HTTP/1.1 {} OK\r\nContent-Type: {}\r\nContent-Length: {}\r\nConnection: close\r\n",
                    reply.status,
                    reply.content_type,
                    reply.body.len()
                );
                for (k, v) in reply.headers {
                    raw.push_str(&format!("{k}: {v}\r\n"));
                }
                raw.push_str("\r\n");
                raw.push_str(&reply.body);
                stream.write_all(raw.as_bytes()).unwrap();
                stream.flush().unwrap();
                let _ = stream.shutdown(std::net::Shutdown::Write);
            }
            requests
        });
        let target = Target {
            host: "127.0.0.1".into(),
            port_offset: bound_port - base_port,
            token: Some("test-token".into()),
            timeout: Duration::from_secs(2),
        };
        (target, handle)
    }

    #[test]
    fn loopback_a2a_card_unknown_and_stream_conformance_checks() {
        let honest_card = json!({
            "name": "susi-gawd",
            "supportedInterfaces": [{"url": "http://127.0.0.1:9094/"}],
            "capabilities": {"streaming": true},
            "skills": [
                {"id": "missions", "name": "missions", "description": "Run a GAWD mission."},
                {"id": "agents", "name": "agents", "description": "Delegate to installed agents."},
                {"id": "frameworks", "name": "frameworks", "description": "Run external frameworks."},
                {"id": "models", "name": "models", "description": "Route through GEMI models."}
            ]
        });
        let dishonest_card = json!({
            "name": "susi-gawd",
            "supportedInterfaces": [{"url": "http://127.0.0.1:9094/"}],
            "capabilities": {"streaming": true},
            "skills": [
                {"id": "missions", "name": "missions", "description": "Renders a desktop UI via egui."},
                {"id": "agents", "name": "agents", "description": "Delegate to installed agents."},
                {"id": "frameworks", "name": "frameworks", "description": "Run external frameworks."},
                {"id": "models", "name": "models", "description": "Route through GEMI models."}
            ]
        });
        let (target, handle) = spawn_loopback_http(
            ports::A2A,
            vec![
                // 1. a2a_card (honest -> Pass)
                MockReply::json(200, &honest_card),
                // 2. a2a_card (dishonest -> Fail)
                MockReply::json(200, &dishonest_card),
                // 3. auth_required POST / (401 -> Pass)
                MockReply::json(401, &json!({"error": "unauthorized"})),
                // 4. auth_required_as POST /stream (401 -> Pass)
                MockReply::json(401, &json!({"error": "unauthorized"})),
                // 5. a2a_unknown (-32601 -> Pass)
                MockReply::json(
                    200,
                    &json!({"jsonrpc": "2.0", "id": 7, "error": {"code": -32601, "message": "Method not found"}}),
                ),
                // 6..=8. a2a_stream (3 requests: unknown -> -32601, SubscribeToTask -> -32001, tasks/resubscribe -> -32001)
                MockReply::json(
                    200,
                    &json!({"jsonrpc": "2.0", "id": 8, "error": {"code": -32601, "message": "Method not found"}}),
                ),
                MockReply::json(
                    200,
                    &json!({"jsonrpc": "2.0", "id": 9, "error": {"code": -32001, "message": "Task not found"}}),
                ),
                MockReply::json(
                    200,
                    &json!({"jsonrpc": "2.0", "id": 10, "error": {"code": -32001, "message": "Task not found"}}),
                ),
            ],
        );

        let c_card_ok = a2a_card(&target);
        assert_eq!(c_card_ok.outcome, Outcome::Pass, "{}", c_card_ok.detail);
        let c_card_bad = a2a_card(&target);
        assert_eq!(c_card_bad.outcome, Outcome::Fail, "{}", c_card_bad.detail);
        assert!(c_card_bad.detail.contains("dishonest skill"));

        let c_auth_root = auth_required(&target, ports::A2A, "POST", "/");
        assert_eq!(c_auth_root.outcome, Outcome::Pass, "{}", c_auth_root.detail);
        let c_auth_stream = auth_required_as(&target, ports::A2A, "POST", "/stream", "/stream");
        assert_eq!(
            c_auth_stream.outcome,
            Outcome::Pass,
            "{}",
            c_auth_stream.detail
        );

        let c_unknown = a2a_unknown(&target);
        assert_eq!(c_unknown.outcome, Outcome::Pass, "{}", c_unknown.detail);

        let c_stream = a2a_stream(&target);
        assert_eq!(c_stream.outcome, Outcome::Pass, "{}", c_stream.detail);

        let requests = handle.join().unwrap();
        assert_eq!(requests.len(), 8);
        assert!(requests[0].starts_with("GET /.well-known/agent-card.json HTTP/1.1"));
        assert!(requests[5].contains("SubscribeToTask") || requests[6].contains("SubscribeToTask"));
        assert!(requests[7].contains("tasks/resubscribe"));
    }

    #[test]
    fn loopback_mcp_and_alias_9093_discover_tools_unauth_and_session_checks() {
        for base in [ports::MCP, ports::MCP_ALIAS] {
            let (target, handle) = spawn_loopback_http(
                base,
                vec![
                    // 1. health
                    MockReply::json(200, &json!({"status": "ok"})),
                    // 2. auth_required POST /mcp -> 401
                    MockReply::json(401, &json!({"error": "unauthorized"})),
                    // 3. mcp_discover
                    MockReply::json(
                        200,
                        &json!({
                            "jsonrpc": "2.0",
                            "id": 1,
                            "result": {
                                "capabilities": {"tools": {}},
                                "_meta": {
                                    "io.modelcontextprotocol/serverInfo": {"name": "susi-gmcp"}
                                }
                            }
                        }),
                    ),
                    // 4. mcp_tools_at
                    MockReply::json(
                        200,
                        &json!({
                            "jsonrpc": "2.0",
                            "id": 2,
                            "result": {"tools": [{"name": "echo"}]}
                        }),
                    ),
                    // 5..=7. mcp_session_at (initialize with Mcp-Session-Id -> notifications/initialized -> tools/list)
                    MockReply {
                        status: 200,
                        content_type: "application/json",
                        headers: vec![("Mcp-Session-Id", "sess-123")],
                        body: json!({
                            "jsonrpc": "2.0",
                            "id": 1,
                            "result": {
                                "protocolVersion": MCP_SESSION,
                                "capabilities": {"tools": {}},
                                "serverInfo": {"name": "susi-gmcp"}
                            }
                        })
                        .to_string(),
                    },
                    MockReply::json(202, &json!({})),
                    MockReply::json(
                        200,
                        &json!({
                            "jsonrpc": "2.0",
                            "id": 2,
                            "result": {"tools": [{"name": "echo"}, {"name": "ping"}]}
                        }),
                    ),
                    // 8. mcp_session_at negative: missing Mcp-Session-Id header on initialize -> Fail
                    MockReply::json(
                        200,
                        &json!({
                            "jsonrpc": "2.0",
                            "id": 1,
                            "result": {"protocolVersion": MCP_SESSION}
                        }),
                    ),
                ],
            );

            assert_eq!(health(&target, base).outcome, Outcome::Pass);
            assert_eq!(
                auth_required(&target, base, "POST", "/mcp").outcome,
                Outcome::Pass
            );
            assert_eq!(mcp_discover(&target, base).outcome, Outcome::Pass);
            assert_eq!(mcp_tools_at(&target, base).outcome, Outcome::Pass);

            let c_sess = mcp_session_at(&target, base);
            assert_eq!(c_sess.outcome, Outcome::Pass, "{}", c_sess.detail);
            assert!(c_sess.detail.contains("2 tools"), "{}", c_sess.detail);

            let c_sess_bad = mcp_session_at(&target, base);
            assert_eq!(c_sess_bad.outcome, Outcome::Fail, "{}", c_sess_bad.detail);
            assert!(c_sess_bad.detail.contains("without Mcp-Session-Id"));

            let requests = handle.join().unwrap();
            assert_eq!(requests.len(), 8);
            assert!(requests[5]
                .to_ascii_lowercase()
                .contains("mcp-session-id: sess-123"));
            assert!(requests[6]
                .to_ascii_lowercase()
                .contains("mcp-session-id: sess-123"));
        }
    }

    #[test]
    fn loopback_gemi_models_unauth_and_sse_stream_checks() {
        let (target, handle) = spawn_loopback_http(
            ports::GEMI,
            vec![
                // 1. auth_required GET /v1/models -> 401
                MockReply::json(401, &json!({"error": "unauthorized"})),
                // 2. gemi_models clean -> Pass
                MockReply::json(
                    200,
                    &json!({"object": "list", "data": [{"id": "susi-1"}, {"id": "qwen2.5-0.5b"}]}),
                ),
                // 3. gemi_models leaking filesystem path -> Warn
                MockReply::json(
                    200,
                    &json!({"object": "list", "data": [{"id": "/home/user/model.gguf"}]}),
                ),
                // 4. gemi_stream valid SSE -> Pass
                MockReply {
                    status: 200,
                    content_type: "text/event-stream",
                    headers: Vec::new(),
                    body: "data: {\"id\":\"1\",\"object\":\"chat.completion.chunk\"}\n\ndata: [DONE]\n\n"
                        .into(),
                },
            ],
        );

        let c_unauth = auth_required(&target, ports::GEMI, "GET", "/v1/models");
        assert_eq!(c_unauth.outcome, Outcome::Pass, "{}", c_unauth.detail);

        let c_models_ok = gemi_models(&target);
        assert_eq!(c_models_ok.outcome, Outcome::Pass, "{}", c_models_ok.detail);
        assert_eq!(c_models_ok.detail, "2 models");

        let c_models_warn = gemi_models(&target);
        assert_eq!(
            c_models_warn.outcome,
            Outcome::Warn,
            "{}",
            c_models_warn.detail
        );
        assert!(c_models_warn.detail.contains("local filesystem paths"));

        let c_stream = gemi_stream(
            &target,
            &Optional {
                inference: true,
                ..Optional::default()
            },
        );
        assert_eq!(c_stream.outcome, Outcome::Pass, "{}", c_stream.detail);
        assert!(c_stream.detail.contains("1 JSON chunks, [DONE] true"));

        let requests = handle.join().unwrap();
        assert_eq!(requests.len(), 4);
        assert!(requests[3].starts_with("POST /v1/chat/completions HTTP/1.1"));
    }

    fn serve_bound_http(
        listener: std::net::TcpListener,
        replies: Vec<MockReply>,
    ) -> std::thread::JoinHandle<usize> {
        use std::io::Write;
        std::thread::spawn(move || {
            let mut count = 0;
            for reply in replies {
                let (mut stream, _) = listener.accept().unwrap();
                let _ = stream.set_read_timeout(Some(Duration::from_secs(2)));
                let _ = stream.set_write_timeout(Some(Duration::from_secs(2)));
                let _ = read_http_request(&mut stream);
                let mut raw = format!(
                    "HTTP/1.1 {} OK\r\nContent-Type: {}\r\nContent-Length: {}\r\nConnection: close\r\n",
                    reply.status,
                    reply.content_type,
                    reply.body.len()
                );
                for (k, v) in reply.headers {
                    raw.push_str(&format!("{k}: {v}\r\n"));
                }
                raw.push_str("\r\n");
                raw.push_str(&reply.body);
                stream.write_all(raw.as_bytes()).unwrap();
                stream.flush().unwrap();
                let _ = stream.shutdown(std::net::Shutdown::Write);
                count += 1;
            }
            count
        })
    }

    #[test]
    fn full_suite_run_with_passes_across_all_five_contract_ports_at_shared_offset() {
        use std::net::{TcpListener, UdpSocket};
        let mut bound = None;
        for offset in (20_000u16..45_000).step_by(10) {
            let l_mcp = TcpListener::bind(("127.0.0.1", ports::MCP + offset));
            let l_gemi = TcpListener::bind(("127.0.0.1", ports::GEMI + offset));
            let u_disc = UdpSocket::bind(("127.0.0.1", ports::UDP_DISCOVERY + offset));
            let l_alias = TcpListener::bind(("127.0.0.1", ports::MCP_ALIAS + offset));
            let l_a2a = TcpListener::bind(("127.0.0.1", ports::A2A + offset));
            if let (Ok(mcp), Ok(gemi), Ok(udp), Ok(alias), Ok(a2a)) =
                (l_mcp, l_gemi, u_disc, l_alias, l_a2a)
            {
                bound = Some((offset, mcp, gemi, udp, alias, a2a));
                break;
            }
        }
        let (offset, mcp_l, gemi_l, udp_s, alias_l, a2a_l) =
            bound.expect("failed to bind 5 contiguous contract ports in 20000..45000");

        let discover_reply = || {
            MockReply::json(
                200,
                &json!({
                    "jsonrpc": "2.0",
                    "id": 1,
                    "result": {
                        "capabilities": {"tools": {}},
                        "_meta": {"io.modelcontextprotocol/serverInfo": {"name": "susi-gmcp"}}
                    }
                }),
            )
        };
        let tools_reply = || {
            MockReply::json(
                200,
                &json!({
                    "jsonrpc": "2.0",
                    "id": 2,
                    "result": {"tools": [{"name": "echo"}]}
                }),
            )
        };
        let session_init_reply = || MockReply {
            status: 200,
            content_type: "application/json",
            headers: vec![("Mcp-Session-Id", "sess-5port")],
            body: json!({
                "jsonrpc": "2.0",
                "id": 1,
                "result": {
                    "protocolVersion": MCP_SESSION,
                    "capabilities": {"tools": {}},
                    "serverInfo": {"name": "susi-gmcp"}
                }
            })
            .to_string(),
        };

        // 1. MCP (9090 + offset): 8 requests (health, auth_required, discover, tools, unknown, 3x session)
        let h_mcp = serve_bound_http(
            mcp_l,
            vec![
                MockReply::json(200, &json!({"status": "ok"})),
                MockReply::json(401, &json!({"error": "unauthorized"})),
                discover_reply(),
                tools_reply(),
                MockReply::json(
                    200,
                    &json!({"jsonrpc": "2.0", "id": 3, "error": {"code": -32601, "message": "Method not found"}}),
                ),
                session_init_reply(),
                MockReply::json(202, &json!({})),
                tools_reply(),
            ],
        );

        // 2. GEMI (9091 + offset): 4 requests (health, auth_required, models, stream)
        let h_gemi = serve_bound_http(
            gemi_l,
            vec![
                MockReply::json(200, &json!({"status": "ok"})),
                MockReply::json(401, &json!({"error": "unauthorized"})),
                MockReply::json(200, &json!({"object": "list", "data": [{"id": "susi-1"}]})),
                MockReply {
                    status: 200,
                    content_type: "text/event-stream",
                    headers: Vec::new(),
                    body: "data: {\"id\":\"1\",\"object\":\"chat.completion.chunk\"}\n\ndata: [DONE]\n\n"
                        .into(),
                },
            ],
        );

        // 3. MCP_ALIAS (9093 + offset): 7 requests (health, auth_required, discover, tools, 3x session)
        let h_alias = serve_bound_http(
            alias_l,
            vec![
                MockReply::json(200, &json!({"status": "ok"})),
                MockReply::json(401, &json!({"error": "unauthorized"})),
                discover_reply(),
                tools_reply(),
                session_init_reply(),
                MockReply::json(202, &json!({})),
                tools_reply(),
            ],
        );

        // 4. A2A + ACP (9094 + offset): 15 requests (health, auth /, auth /stream,
        // auth /acp, auth /acp/stream, card, a2a unknown, 3x a2a stream,
        // acp discover, 4x acp rpc)
        let honest_card = json!({
            "name": "susi-gawd",
            "supportedInterfaces": [{"url": "http://127.0.0.1:9094/"}],
            "capabilities": {"streaming": true},
            "skills": [
                {"id": "missions", "name": "missions", "description": "Run a GAWD mission."},
                {"id": "agents", "name": "agents", "description": "Delegate to installed agents."},
                {"id": "frameworks", "name": "frameworks", "description": "Run external frameworks."},
                {"id": "models", "name": "models", "description": "Route through GEMI models."}
            ]
        });
        let honest_acp_discovery = json!({
            "schema": ACP_DISCOVERY_SCHEMA,
            "protocol_version": ACP_PROTOCOL,
            "endpoints": {
                "rpc": "/acp",
                "stream": "/acp/stream"
            }
        });
        let h_a2a = serve_bound_http(
            a2a_l,
            vec![
                MockReply::json(200, &json!({"status": "ok"})),
                MockReply::json(401, &json!({"error": "unauthorized"})),
                MockReply::json(401, &json!({"error": "unauthorized"})),
                MockReply::json(401, &json!({"error": "unauthorized"})),
                MockReply::json(401, &json!({"error": "unauthorized"})),
                MockReply::json(200, &honest_card),
                MockReply::json(
                    200,
                    &json!({"jsonrpc": "2.0", "id": 7, "error": {"code": -32601, "message": "Method not found"}}),
                ),
                MockReply::json(
                    200,
                    &json!({"jsonrpc": "2.0", "id": 8, "error": {"code": -32601, "message": "Method not found"}}),
                ),
                MockReply::json(
                    200,
                    &json!({"jsonrpc": "2.0", "id": 9, "error": {"code": -32001, "message": "Task not found"}}),
                ),
                MockReply::json(
                    200,
                    &json!({"jsonrpc": "2.0", "id": 10, "error": {"code": -32001, "message": "Task not found"}}),
                ),
                MockReply::json(200, &honest_acp_discovery),
                MockReply::json(
                    200,
                    &json!({"jsonrpc": "2.0", "id": 1, "result": {"protocol_version": ACP_PROTOCOL}}),
                ),
                MockReply::json(
                    200,
                    &json!({"jsonrpc": "2.0", "id": 2, "result": {"session_id": "acp-sess-000001"}}),
                ),
                MockReply::json(
                    200,
                    &json!({"jsonrpc": "2.0", "id": 3, "result": {"session_id": "acp-sess-000001", "closed": true}}),
                ),
                MockReply::json(
                    200,
                    &json!({"jsonrpc": "2.0", "id": 4, "error": {"code": -32601, "message": "Method not found"}}),
                ),
            ],
        );

        // 5. UDP_DISCOVERY (9092 + offset): every unauthenticated datagram
        // (silent) + 1 signed datagram (pong)
        let h_udp = std::thread::spawn(move || {
            let _ = udp_s.set_read_timeout(Some(Duration::from_secs(3)));
            let mut buf = [0u8; 256];
            for _ in 0..=UNAUTHENTICATED_PINGS.len() {
                if let Ok((n, src)) = udp_s.recv_from(&mut buf) {
                    let msg = String::from_utf8_lossy(&buf[..n]);
                    if msg.starts_with("SUSI_PING_SIG2:susi-conformance:CORE:9090:1:") {
                        let _ = udp_s.send_to(
                            b"SUSI_PONG_SIG2:host:CORE:9090:1:deadbeef:127.0.0.1:cafebabe",
                            src,
                        );
                    }
                }
            }
        });

        let target = Target {
            host: "127.0.0.1".into(),
            port_offset: offset,
            token: Some("test-token".into()),
            timeout: Duration::from_millis(250),
        };
        let optional = Optional {
            susi_bin: None,
            inference: true,
            signed_udp_ping: Some(
                "SUSI_PING_SIG2:susi-conformance:CORE:9090:1:deadbeef:127.0.0.1:cafebabe".into(),
            ),
            signed_udp_pong_prefix: Some("SUSI_PONG_SIG2:host:CORE:".into()),
        };

        let checks = run_with(&target, &optional);
        assert!(conformant(&checks), "{checks:?}");
        for check in &checks {
            if matches!(check.id.as_str(), "mcp.stdio" | "acp.stdio") {
                assert_eq!(check.outcome, Outcome::Skip, "{}", check.detail);
            } else {
                assert_eq!(
                    check.outcome,
                    Outcome::Pass,
                    "{}: {}",
                    check.id,
                    check.detail
                );
            }
        }

        assert_eq!(h_mcp.join().unwrap(), 8);
        assert_eq!(h_gemi.join().unwrap(), 4);
        assert_eq!(h_alias.join().unwrap(), 7);
        assert_eq!(h_a2a.join().unwrap(), 15);
        h_udp.join().unwrap();
    }

    #[test]
    fn loopback_acp_discover_auth_and_rpc_conformance_checks() {
        let valid_discovery = json!({
            "schema": ACP_DISCOVERY_SCHEMA,
            "protocol_version": ACP_PROTOCOL,
            "endpoints": {
                "rpc": "/acp",
                "stream": "/acp/stream"
            }
        });
        let bad_schema_discovery = json!({
            "schema": "wrong/v0",
            "protocol_version": ACP_PROTOCOL,
            "endpoints": {
                "rpc": "/acp",
                "stream": "/acp/stream"
            }
        });
        let missing_stream_discovery = json!({
            "schema": ACP_DISCOVERY_SCHEMA,
            "protocol_version": ACP_PROTOCOL,
            "endpoints": {
                "rpc": "/acp"
            }
        });

        let (target, handle) = spawn_loopback_http(
            ports::A2A,
            vec![
                // 1. acp_discover valid -> Pass
                MockReply::json(200, &valid_discovery),
                // 2. acp_discover wrong schema -> Fail
                MockReply::json(200, &bad_schema_discovery),
                // 3. acp_discover missing /acp/stream -> Fail
                MockReply::json(200, &missing_stream_discovery),
                // 4. auth_required_as POST /acp -> 401 Pass
                MockReply::json(401, &json!({"error": "unauthorized"})),
                // 5. auth_required_as POST /acp/stream -> 401 Pass
                MockReply::json(401, &json!({"error": "unauthorized"})),
                // 6..=9. acp_rpc full lifecycle (initialize, session/new, session/close, unknown -> -32601)
                MockReply::json(
                    200,
                    &json!({"jsonrpc": "2.0", "id": 1, "result": {"protocolVersion": ACP_PROTOCOL}}),
                ),
                MockReply::json(
                    200,
                    &json!({"jsonrpc": "2.0", "id": 2, "result": {"sessionId": "acp-sess-42"}}),
                ),
                MockReply::json(
                    200,
                    &json!({"jsonrpc": "2.0", "id": 3, "result": {"session_id": "acp-sess-42", "closed": true}}),
                ),
                MockReply::json(
                    200,
                    &json!({"jsonrpc": "2.0", "id": 4, "error": {"code": -32601, "message": "Method not found"}}),
                ),
                // 10..=13. acp_rpc failing when unknown method does not return -32601
                MockReply::json(
                    200,
                    &json!({"jsonrpc": "2.0", "id": 1, "result": {"protocol_version": ACP_PROTOCOL}}),
                ),
                MockReply::json(
                    200,
                    &json!({"jsonrpc": "2.0", "id": 2, "result": {"session_id": "acp-sess-43"}}),
                ),
                MockReply::json(
                    200,
                    &json!({"jsonrpc": "2.0", "id": 3, "result": {"session_id": "acp-sess-43", "closed": true}}),
                ),
                MockReply::json(
                    200,
                    &json!({"jsonrpc": "2.0", "id": 4, "result": {"unexpected": true}}),
                ),
            ],
        );

        let c_disc_ok = acp_discover(&target);
        assert_eq!(c_disc_ok.outcome, Outcome::Pass, "{}", c_disc_ok.detail);
        assert!(
            c_disc_ok.detail.contains(ACP_PROTOCOL),
            "{}",
            c_disc_ok.detail
        );

        let c_disc_bad_schema = acp_discover(&target);
        assert_eq!(
            c_disc_bad_schema.outcome,
            Outcome::Fail,
            "{}",
            c_disc_bad_schema.detail
        );
        assert!(c_disc_bad_schema.detail.contains("wrong/v0"));

        let c_disc_missing_stream = acp_discover(&target);
        assert_eq!(
            c_disc_missing_stream.outcome,
            Outcome::Fail,
            "{}",
            c_disc_missing_stream.detail
        );
        assert!(
            c_disc_missing_stream
                .detail
                .contains("endpoints=(\"/acp\", \"\")"),
            "{}",
            c_disc_missing_stream.detail
        );

        let c_auth_acp = auth_required_as(&target, ports::A2A, "POST", "/acp", "/acp");
        assert_eq!(c_auth_acp.outcome, Outcome::Pass, "{}", c_auth_acp.detail);

        let c_auth_stream =
            auth_required_as(&target, ports::A2A, "POST", "/acp/stream", "/acp/stream");
        assert_eq!(
            c_auth_stream.outcome,
            Outcome::Pass,
            "{}",
            c_auth_stream.detail
        );

        let mut no_token_target = target.clone();
        no_token_target.token = None;
        let c_rpc_skip = acp_rpc(&no_token_target);
        assert_eq!(c_rpc_skip.outcome, Outcome::Skip, "{}", c_rpc_skip.detail);

        let c_rpc_ok = acp_rpc(&target);
        assert_eq!(c_rpc_ok.outcome, Outcome::Pass, "{}", c_rpc_ok.detail);
        assert!(
            c_rpc_ok.detail.contains("acp-sess-42"),
            "{}",
            c_rpc_ok.detail
        );

        let c_rpc_bad_unknown = acp_rpc(&target);
        assert_eq!(
            c_rpc_bad_unknown.outcome,
            Outcome::Fail,
            "{}",
            c_rpc_bad_unknown.detail
        );
        assert!(c_rpc_bad_unknown.detail.contains("-32601"));

        let requests = handle.join().unwrap();
        assert_eq!(requests.len(), 13);
        assert!(requests[0].starts_with("GET /.well-known/acp.json HTTP/1.1"));
        assert!(requests[3].starts_with("POST /acp HTTP/1.1"));
        assert!(requests[4].starts_with("POST /acp/stream HTTP/1.1"));
        assert!(requests[5].contains("\"initialize\""));
        assert!(requests[6].contains("\"session/new\""));
        assert!(requests[7].contains("\"session/close\""));
        assert!(requests[8].contains("\"acp/no_such_method\""));
    }

    #[test]
    fn acp_stdio_session_and_subprocess_reject_banner_and_validate_lifecycle() {
        let wait = Duration::from_millis(200);

        // 1. Valid initialize -> session/new -> session/list -> session/close passes.
        let (tx, rx) = std::sync::mpsc::channel();
        tx.send(
            json!({"jsonrpc": "2.0", "id": 1, "result": {"protocol_version": ACP_PROTOCOL}})
                .to_string(),
        )
        .unwrap();
        tx.send(
            json!({"jsonrpc": "2.0", "id": 2, "result": {"session_id": "acp-sess-000001"}})
                .to_string(),
        )
        .unwrap();
        tx.send(
            json!({
                "jsonrpc": "2.0",
                "id": 3,
                "result": {"sessions": [{"session_id": "acp-sess-000001"}]}
            })
            .to_string(),
        )
        .unwrap();
        tx.send(
            json!({
                "jsonrpc": "2.0",
                "id": 4,
                "result": {"session_id": "acp-sess-000001", "closed": true}
            })
            .to_string(),
        )
        .unwrap();
        let mut stdin_buf = Vec::new();
        let (negotiated, session_id) = acp_stdio_session(&mut stdin_buf, &rx, wait, wait).unwrap();
        assert_eq!(negotiated, ACP_PROTOCOL);
        assert_eq!(session_id, "acp-sess-000001");
        let sent = String::from_utf8(stdin_buf).unwrap();
        let sent_lines: Vec<&str> = sent.lines().collect();
        assert_eq!(sent_lines.len(), 4);
        assert!(sent_lines[0].contains("\"initialize\""));
        assert!(sent_lines[1].contains("\"session/new\""));
        assert!(sent_lines[2].contains("\"session/list\""));
        assert!(sent_lines[3].contains("\"session/close\""));

        // 2. Non-JSON banner on stdout fails immediately.
        let (tx_banner, rx_banner) = std::sync::mpsc::channel();
        tx_banner
            .send("[SUSI] bootloader banner leaked on stdout".to_string())
            .unwrap();
        let mut sink = Vec::new();
        let banner_err = acp_stdio_session(&mut sink, &rx_banner, wait, wait).unwrap_err();
        assert!(banner_err.contains("bootloader banner"), "{banner_err}");

        // 3. session/list omitting created session fails.
        let (tx_list, rx_list) = std::sync::mpsc::channel();
        tx_list
            .send(
                json!({"jsonrpc": "2.0", "id": 1, "result": {"protocol_version": ACP_PROTOCOL}})
                    .to_string(),
            )
            .unwrap();
        tx_list
            .send(
                json!({"jsonrpc": "2.0", "id": 2, "result": {"session_id": "acp-sess-000009"}})
                    .to_string(),
            )
            .unwrap();
        tx_list
            .send(json!({"jsonrpc": "2.0", "id": 3, "result": {"sessions": []}}).to_string())
            .unwrap();
        let mut sink2 = Vec::new();
        let list_err = acp_stdio_session(&mut sink2, &rx_list, wait, wait).unwrap_err();
        assert!(
            list_err.contains("did not include created session"),
            "{list_err}"
        );

        // 4. acp_stdio subprocess spawn failure & non-ACP binary failure reap cleanly.
        let target = Target {
            host: "127.0.0.1".into(),
            port_offset: 0,
            token: None,
            timeout: Duration::from_millis(200),
        };
        let missing = acp_stdio(
            &target,
            &Optional {
                susi_bin: Some(PathBuf::from("/nonexistent-susi-acp-binary")),
                ..Optional::default()
            },
        );
        assert_eq!(missing.outcome, Outcome::Fail, "{}", missing.detail);
        assert!(missing.detail.contains("spawn"), "{}", missing.detail);
    }
}

//! Compatibility adapters for the shared protocol client and host HTTP transport.

use crate::tools::{Policy, ToolSpec};
use serde_json::Value;
use std::process::{Child, Stdio};
use std::time::Duration;
use susi_config::mcp_client as shared;
use susi_error::{EaiError, EaiResult};

pub use shared::{
    is_supported_protocol_version, Transport, DEFAULT_STDIO_TIMEOUT_SECS, MAX_LIST_PAGES,
    MAX_STDIO_FRAME_BYTES, PROTOCOL_VERSION, SUPPORTED_PROTOCOL_VERSIONS,
};

/// Default GMCP streamable-HTTP path on the Susi OS Socket.
pub const GMCP_HTTP_PATH: &str = "/mcp";

/// Largest response body [`HttpSocketTransport`] reads.
pub const MAX_HTTP_RESPONSE_BYTES: u64 = 8 * 1024 * 1024;

/// One tool an MCP server advertises.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct McpTool {
    /// Name to call.
    pub name: String,
    /// Description to show the model.
    pub description: String,
    /// JSON schema of the arguments.
    pub schema: Value,
}

impl McpTool {
    /// Turn the server's advertisement into a registry entry.
    #[must_use]
    pub fn to_spec(&self, policy: Policy) -> ToolSpec {
        ToolSpec::mcp(&self.name, &self.description, self.schema.clone(), policy)
    }
}

impl From<shared::McpTool> for McpTool {
    fn from(tool: shared::McpTool) -> Self {
        Self {
            name: tool.name,
            description: tool.description,
            schema: tool.schema,
        }
    }
}

/// Preserve the harness advertisement type while sharing protocol session state.
#[derive(Debug)]
pub struct McpClient<T: Transport>(shared::McpClient<T>);
impl<T: Transport> McpClient<T> {
    pub fn new(transport: T) -> Self {
        Self(
            shared::McpClient::new(transport).with_client_info(shared::ClientInfo {
                name: "susi-harness",
                version: env!("CARGO_PKG_VERSION"),
            }),
        )
    }
    pub fn list_tools(&mut self) -> EaiResult<Vec<McpTool>> {
        self.0
            .list_tools()
            .map(|tools| tools.into_iter().map(Into::into).collect())
    }
    pub fn close(&mut self) -> EaiResult<()> {
        self.0.close()
    }
    pub fn negotiated_protocol_version(&self) -> Option<&str> {
        self.0.negotiated_protocol_version()
    }
    pub fn negotiated_supported(&self) -> bool {
        self.0.negotiated_supported()
    }
    pub fn transport(&self) -> &T {
        self.0.transport()
    }
    pub fn set_call_timeout(&mut self, timeout: Option<Duration>) {
        self.0.set_call_timeout(timeout);
    }
    pub fn initialize(&mut self) -> EaiResult<Value> {
        self.0.initialize()
    }
    pub fn set_timeout(&mut self, timeout: Duration) {
        self.0.set_timeout(timeout);
    }
    pub fn call_result(&mut self, name: &str, arguments: &Value) -> EaiResult<Value> {
        self.0.call_result(name, arguments)
    }
    pub fn read_resource(&mut self, uri: &str) -> EaiResult<Value> {
        self.0.read_resource(uri)
    }
    pub fn call(&mut self, name: &str, arguments: &Value) -> EaiResult<String> {
        self.0.call(name, arguments)
    }
}
/// Harness process preparation around the shared owned stdio transport.
#[derive(Debug)]
pub struct StdioTransport(shared::StdioTransport);
impl StdioTransport {
    /// Spawn an MCP server and connect to it.
    ///
    /// # Errors
    ///
    /// Returns an I/O error when the process cannot be spawned or its pipes
    /// cannot be taken.
    pub fn spawn(program: &str, args: &[String]) -> EaiResult<Self> {
        let mut cmd = susi_paths::console::background_command(program);
        cmd.args(args)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::null());
        if crate::workload::enabled() {
            crate::workload::clean_environment(&mut cmd);
        } else {
            for (k, v) in susi_config::cloud_env_overlay() {
                if std::env::var_os(&k).is_none() {
                    cmd.env(k, v);
                }
            }
        }
        cmd.env("SUSI_UNATTENDED", "1");
        susi_paths::console::apply_non_interactive_env(&mut cmd);
        let child = cmd
            .spawn()
            .map_err(|error| EaiError::io(format!("mcp spawn {program}: {error}")))?;
        Self::from_child(child)
    }

    pub fn from_child(child: Child) -> EaiResult<Self> {
        shared::StdioTransport::from_child(child).map(Self)
    }
    pub fn with_timeout(self, timeout: Duration) -> Self {
        Self(self.0.with_timeout(timeout))
    }
    pub fn with_timeout_secs(self, seconds: u64) -> Self {
        Self(self.0.with_timeout_secs(seconds))
    }
}
impl Transport for StdioTransport {
    fn exchange(&mut self, request: &Value) -> EaiResult<Value> {
        self.0.exchange(request)
    }
    fn notify(&mut self, request: &Value) -> EaiResult<()> {
        self.0.notify(request)
    }
    fn set_protocol_version(&mut self, version: &str) {
        self.0.set_protocol_version(version);
    }
    fn set_timeout(&mut self, timeout: Duration) {
        self.0.set_timeout(timeout);
    }
    fn set_call_timeout(&mut self, timeout: Option<Duration>) {
        self.0.set_call_timeout(timeout);
    }
    fn close(&mut self) -> EaiResult<()> {
        self.0.close()
    }
}

/// A JSON-RPC transport over MCP streamable HTTP, for the GMCP socket of the
/// Susi AI OS (`http://127.0.0.1:9090/mcp` by default).
///
/// Each message is one `POST`. The `Mcp-Session-Id` the server returns on
/// `initialize` is echoed on every later message, and once the client has
/// negotiated a revision it is sent as `MCP-Protocol-Version`. A response may
/// be plain JSON or a `text/event-stream`; the JSON-RPC answer is taken from
/// the stream's `data:` lines.
#[derive(Debug, Clone)]
pub struct HttpSocketTransport {
    url: String,
    timeout_secs: u64,
    bearer: Option<crate::secrets::Credential>,
    session_id: Option<String>,
    protocol_version: Option<String>,
    headers: Vec<(String, String)>,
}

impl HttpSocketTransport {
    /// A transport to `url`. A URL with no path (`http://127.0.0.1:9090`) gets
    /// the GMCP path [`GMCP_HTTP_PATH`] appended; any explicit path is kept.
    #[must_use]
    pub fn new(url: &str) -> Self {
        Self {
            url: endpoint_url(url),
            timeout_secs: 60,
            bearer: None,
            session_id: None,
            protocol_version: None,
            headers: Vec::new(),
        }
    }

    /// The transport for the Susi OS Socket's GMCP endpoint.
    #[must_use]
    pub fn for_socket(endpoints: &crate::adaptive::SusiSocketEndpoints) -> Self {
        Self::new(&endpoints.gmcp_http_url)
    }

    /// Set the per-message deadline (at least one second).
    #[must_use]
    pub fn with_timeout_secs(mut self, timeout_secs: u64) -> Self {
        self.timeout_secs = timeout_secs.max(1);
        self
    }

    /// Add application headers without allowing protocol/framing overrides.
    pub fn with_headers(mut self, headers: Vec<(String, String)>) -> EaiResult<Self> {
        for (name, value) in &headers {
            if name.is_empty()
                || !name
                    .bytes()
                    .all(|byte| byte.is_ascii_alphanumeric() || b"!#$%&'*+-.^_`|~".contains(&byte))
                || value.bytes().any(|byte| byte == b'\r' || byte == b'\n')
                || matches!(
                    name.to_ascii_lowercase().as_str(),
                    "host"
                        | "content-length"
                        | "transfer-encoding"
                        | "connection"
                        | "content-type"
                        | "accept"
                        | "mcp-session-id"
                        | "mcp-protocol-version"
                )
            {
                return Err(EaiError::config("invalid or reserved MCP HTTP header"));
            }
        }
        self.headers = headers;
        Ok(self)
    }

    /// Send `Authorization: Bearer <token>` on every message.
    #[must_use]
    pub fn with_bearer(mut self, token: impl Into<String>) -> Self {
        self.bearer = Some(crate::secrets::Credential::from(token.into()));
        self
    }

    /// Resolve this bearer credential before each HTTP operation.
    #[must_use]
    pub fn with_bearer_env(mut self, name: impl Into<String>) -> Self {
        self.bearer = Some(crate::secrets::Credential::Named(name.into()));
        self
    }

    fn authorization(&self) -> EaiResult<Option<String>> {
        self.bearer
            .as_ref()
            .map(|source| {
                let key = source
                    .resolve()
                    .ok_or_else(|| EaiError::config("MCP bearer credential unavailable"))?;
                Ok(format!("Bearer {}", key.expose()))
            })
            .transpose()
    }

    /// The endpoint every message is posted to.
    #[must_use]
    pub fn url(&self) -> &str {
        &self.url
    }

    /// The session the server assigned, once it has assigned one.
    #[must_use]
    pub fn session_id(&self) -> Option<&str> {
        self.session_id.as_deref()
    }

    fn post(&mut self, message: &Value) -> EaiResult<susi_http_transport::HttpCall> {
        let body = serde_json::to_vec(message)
            .map_err(|error| EaiError::internal(format!("mcp http request: {error}")))?;
        let authorization = self.authorization()?;
        let mut headers: Vec<(&str, &str)> = vec![
            ("content-type", "application/json"),
            ("accept", "application/json, text/event-stream"),
        ];
        if let Some(session) = self.session_id.as_deref() {
            headers.push(("mcp-session-id", session));
        }
        if let Some(version) = self.protocol_version.as_deref() {
            headers.push(("mcp-protocol-version", version));
        }
        if let Some(authorization) = authorization.as_deref() {
            headers.push(("authorization", authorization));
        }
        headers.extend(
            self.headers
                .iter()
                .map(|(name, value)| (name.as_str(), value.as_str())),
        );
        let call = susi_http_transport::http_call_with_body(
            "POST",
            &self.url,
            &headers,
            Some(&body),
            self.timeout_secs,
            0,
        )
        .map_err(|error| EaiError::network(format!("mcp http {}: {error}", self.url)))?;
        if let Some(session) = call.header("mcp-session-id") {
            self.session_id = Some(session.to_string());
        }
        Ok(call)
    }
}

impl Transport for HttpSocketTransport {
    fn set_timeout(&mut self, timeout: Duration) {
        self.timeout_secs = timeout.as_secs().max(1);
    }

    fn exchange(&mut self, request: &Value) -> EaiResult<Value> {
        let call = self.post(request)?;
        let status = call.status;
        let event_stream = call
            .header("content-type")
            .is_some_and(|kind| kind.to_ascii_lowercase().contains("text/event-stream"));
        let text = call
            .into_utf8(MAX_HTTP_RESPONSE_BYTES)
            .map_err(|error| EaiError::network(format!("mcp http body: {error}")))?;
        if !(200..300).contains(&status) {
            if self.bearer.is_some() && crate::workload::enabled() {
                return Err(EaiError::network(format!(
                    "MCP endpoint returned HTTP {status}"
                )));
            }
            return Err(EaiError::network(format!(
                "mcp http {} returned HTTP {status}: {}",
                self.url,
                text.chars().take(500).collect::<String>()
            )));
        }
        if event_stream {
            return response_from_event_stream(&text, request.get("id"));
        }
        if text.trim().is_empty() {
            return Err(EaiError::network("mcp http response has no body"));
        }
        susi_config::protocol_json::parse(text.as_bytes())
            .map_err(|error| EaiError::internal(format!("mcp http response: {error}")))
    }

    fn notify(&mut self, notification: &Value) -> EaiResult<()> {
        let call = self.post(notification)?;
        if (200..300).contains(&call.status) {
            Ok(())
        } else {
            Err(EaiError::network(format!(
                "mcp http {} refused a notification with HTTP {}",
                self.url, call.status
            )))
        }
    }

    fn set_protocol_version(&mut self, version: &str) {
        self.protocol_version = Some(version.to_string());
    }

    fn close(&mut self) -> EaiResult<()> {
        let Some(session) = self.session_id.take() else {
            return Ok(());
        };
        let authorization = self.authorization()?;
        let mut headers: Vec<(&str, &str)> = vec![("mcp-session-id", session.as_str())];
        if let Some(version) = self.protocol_version.as_deref() {
            headers.push(("mcp-protocol-version", version));
        }
        if let Some(authorization) = authorization.as_deref() {
            headers.push(("authorization", authorization));
        }
        headers.extend(
            self.headers
                .iter()
                .map(|(name, value)| (name.as_str(), value.as_str())),
        );
        let call = susi_http_transport::http_call_with_body(
            "DELETE",
            &self.url,
            &headers,
            None,
            self.timeout_secs,
            0,
        )
        .map_err(|error| EaiError::network(format!("mcp http {}: {error}", self.url)));
        match call {
            Ok(call) if (200..300).contains(&call.status) || call.status == 405 => {
                self.protocol_version = None;
                Ok(())
            }
            Ok(call) => {
                self.session_id = Some(session);
                Err(EaiError::network(format!(
                    "mcp http {} refused session close with HTTP {}",
                    self.url, call.status
                )))
            }
            Err(error) => {
                self.session_id = Some(session);
                Err(error)
            }
        }
    }
}

/// `url` with the GMCP path appended when it names only a host and port.
fn endpoint_url(url: &str) -> String {
    let trimmed = url.trim().trim_end_matches('/');
    let after_scheme = trimmed.split_once("://").map_or(trimmed, |(_, rest)| rest);
    if after_scheme.contains('/') {
        trimmed.to_string()
    } else {
        format!("{trimmed}{GMCP_HTTP_PATH}")
    }
}

/// The JSON-RPC response in a server-sent event stream: the event whose `id`
/// matches the request's, else the last JSON message carrying a result or an
/// error.
fn response_from_event_stream(text: &str, id: Option<&Value>) -> EaiResult<Value> {
    let messages = susi_core::mcp_client::event_stream_messages(
        text.lines().map(|line| line.trim_end_matches('\r')),
    );
    let mut last = None;
    for message in
        messages.filter(|message| message.get("result").is_some() || message.get("error").is_some())
    {
        if id.is_some_and(|id| message.get("id") == Some(id)) {
            return Ok(message);
        }
        last = Some(message);
    }
    last.ok_or_else(|| EaiError::network("mcp event stream carried no JSON-RPC response"))
}

#[cfg(test)]
#[path = "tests/mcp.rs"]
mod tests;

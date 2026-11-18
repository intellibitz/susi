//! The streamable-HTTP MCP transport the composition root registers for
//! `susi_config::mcp_session` (moved from `susi-harness` `mcp.rs`, which
//! leaves core for susi-commercial-apps; core keeps transport). Every message
//! is a POST to the endpoint; the `Mcp-Session-Id` the server assigns rides on
//! later requests, and a reply may come as JSON or as a server-sent event stream.

use serde_json::Value;
use std::time::Duration;
use susi_config::mcp_client::Transport;
use susi_error::{EaiError, EaiResult};

/// Default MCP path appended to a URL that names only a host and port.
pub const GMCP_HTTP_PATH: &str = "/mcp";
/// Largest HTTP response body read from an MCP server.
pub const MAX_HTTP_RESPONSE_BYTES: u64 = 8 * 1024 * 1024;

pub struct HttpTransport {
    url: String,
    timeout_secs: u64,
    session_id: Option<String>,
    protocol_version: Option<String>,
    headers: Vec<(String, String)>,
}

/// Register this transport for HTTP MCP servers reached through `mcp_session`.
pub fn register() {
    susi_config::mcp_session::register_http_transport(|server| {
        let headers = susi_config::mcp::validate_http_headers(server).map_err(EaiError::config)?;
        Ok(Box::new(
            HttpTransport::new(&server.command)
                .with_headers(headers)?
                .with_timeout_secs(12),
        ))
    });
}

impl HttpTransport {
    /// A transport to `url`; a URL with no path gets [`GMCP_HTTP_PATH`].
    #[must_use]
    pub fn new(url: &str) -> Self {
        Self {
            url: endpoint_url(url),
            timeout_secs: 60,
            session_id: None,
            protocol_version: None,
            headers: Vec::new(),
        }
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

    fn headers<'a>(&'a self, session: Option<&'a str>) -> Vec<(&'a str, &'a str)> {
        let mut headers: Vec<(&str, &str)> = Vec::new();
        if let Some(session) = session {
            headers.push(("mcp-session-id", session));
        }
        if let Some(version) = self.protocol_version.as_deref() {
            headers.push(("mcp-protocol-version", version));
        }
        headers.extend(
            self.headers
                .iter()
                .map(|(name, value)| (name.as_str(), value.as_str())),
        );
        headers
    }

    fn post(&mut self, message: &Value) -> EaiResult<susi_http_transport::HttpCall> {
        let body = serde_json::to_vec(message)
            .map_err(|error| EaiError::internal(format!("mcp http request: {error}")))?;
        let mut headers: Vec<(&str, &str)> = vec![
            ("content-type", "application/json"),
            ("accept", "application/json, text/event-stream"),
        ];
        headers.extend(self.headers(self.session_id.as_deref()));
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

impl Transport for HttpTransport {
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
        let headers = self.headers(Some(session.as_str()));
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

/// `url` with the MCP path appended when it names only a host and port.
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
#[path = "tests/mcp_http_transport.rs"]
mod tests;

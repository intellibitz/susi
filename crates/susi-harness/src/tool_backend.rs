//! Resolve installed tools through the existing authorized MCP registry.

use crate::mcp::{HttpSocketTransport, McpClient, StdioTransport, Transport};
use serde_json::Value;
use std::path::Path;
use std::time::Duration;
use susi_config::mac_policy::MacPolicy;
use susi_config::mcp::{self, McpServerConfig};
use susi_error::{EaiError, EaiResult};

enum Channel {
    Stdio(StdioTransport),
    Http(HttpSocketTransport),
}

impl Transport for Channel {
    fn exchange(&mut self, request: &Value) -> EaiResult<Value> {
        match self {
            Self::Stdio(peer) => peer.exchange(request),
            Self::Http(peer) => peer.exchange(request),
        }
    }
    fn notify(&mut self, request: &Value) -> EaiResult<()> {
        match self {
            Self::Stdio(peer) => peer.notify(request),
            Self::Http(peer) => peer.notify(request),
        }
    }
    fn set_protocol_version(&mut self, version: &str) {
        match self {
            Self::Stdio(peer) => peer.set_protocol_version(version),
            Self::Http(peer) => peer.set_protocol_version(version),
        }
    }
    fn set_timeout(&mut self, timeout: Duration) {
        match self {
            Self::Stdio(peer) => peer.set_timeout(timeout),
            Self::Http(peer) => peer.set_timeout(timeout),
        }
    }
    fn close(&mut self) -> EaiResult<()> {
        match self {
            Self::Stdio(peer) => peer.close(),
            Self::Http(peer) => peer.close(),
        }
    }
}

fn connect(server: &McpServerConfig, cwd: Option<&Path>) -> EaiResult<McpClient<Channel>> {
    let policy = MacPolicy::global();
    mcp::authorize_mcp_transport(server, policy).map_err(EaiError::config)?;
    let channel = if server.http_scope().map_err(EaiError::config)?.is_some() {
        if cwd.is_some() {
            return Err(EaiError::config(
                "a process working directory requires a stdio MCP provider",
            ));
        }
        Channel::Http(
            HttpSocketTransport::new(&server.command)
                .with_headers(mcp::validate_http_headers(server).map_err(EaiError::config)?)?
                .with_timeout_secs(12),
        )
    } else {
        let mut command =
            mcp::prepare_authorized_stdio_command(server, policy).map_err(EaiError::config)?;
        if let Some(cwd) = cwd {
            command.current_dir(cwd);
        }
        let child = command
            .spawn()
            .map_err(|error| EaiError::io(format!("MCP provider spawn: {error}")))?;
        Channel::Stdio(StdioTransport::from_child(child)?.with_timeout_secs(12))
    };
    Ok(McpClient::new(channel))
}

/// Call a uniquely advertised tool once. No failed mutation is retried.
pub(crate) fn call_result(name: &str, arguments: &Value, cwd: Option<&Path>) -> EaiResult<Value> {
    if name.is_empty() || !arguments.is_object() {
        return Err(EaiError::config(
            "MCP tool name and object arguments are required",
        ));
    }
    let registry = mcp::read_mcp_config(&mcp::mcp_config_path()).map_err(EaiError::config)?;
    let mut servers: Vec<_> = registry.mcp_servers.into_iter().collect();
    servers.sort_by(|left, right| left.0.cmp(&right.0));
    let mut selected = None;
    let mut unavailable = 0usize;
    for (_, server) in servers {
        let mut client = match connect(&server, cwd) {
            Ok(client) => client,
            Err(_) => {
                unavailable += 1;
                continue;
            }
        };
        let tools = match client.list_tools() {
            Ok(tools) => tools,
            Err(_) => {
                unavailable += 1;
                continue;
            }
        };
        let matches = tools.iter().filter(|tool| tool.name == name).count();
        if matches > 1 || (matches == 1 && selected.is_some()) {
            return Err(EaiError::config(format!(
                "MCP tool '{name}' has ambiguous providers"
            )));
        }
        if matches == 1 {
            selected = Some(client);
        }
    }
    let mut client = selected.ok_or_else(|| {
        EaiError::config(format!(
            "no available authorized MCP provider advertises '{name}' ({unavailable} unavailable)"
        ))
    })?;
    client.set_timeout(Duration::from_secs(600));
    let result = client.call_result(name, arguments);
    // Closing is cleanup, never a reason to replay a completed tool call.
    let _ = client.close();
    result
}

fn json_result(result: Value) -> EaiResult<Value> {
    if result
        .get("isError")
        .is_some_and(|flag| flag != &Value::Bool(false))
    {
        return Err(EaiError::internal("MCP provider returned a tool error"));
    }
    if let Some(structured) = result.get("structuredContent") {
        if !structured.is_object() {
            return Err(EaiError::internal(
                "MCP structured content is not an object",
            ));
        }
        return Ok(structured.clone());
    }
    let blocks = result
        .get("content")
        .and_then(Value::as_array)
        .filter(|blocks| blocks.len() == 1)
        .ok_or_else(|| EaiError::internal("MCP provider returned no unambiguous JSON content"))?;
    if blocks[0].get("type").and_then(Value::as_str) != Some("text") {
        return Err(EaiError::internal("MCP provider returned non-JSON content"));
    }
    let text = blocks[0]
        .get("text")
        .and_then(Value::as_str)
        .ok_or_else(|| EaiError::internal("MCP provider returned malformed text content"))?;
    serde_json::from_str(text)
        .map_err(|error| EaiError::internal(format!("MCP tool JSON: {error}")))
}

/// Read structured content from a registered, authorized standard MCP tool.
pub(crate) fn call_json(name: &str, arguments: &Value) -> EaiResult<Value> {
    json_result(call_result(name, arguments, None)?)
}

/// Call a local stdio tool with an OS-native working directory for this session.
pub(crate) fn call_json_in(name: &str, arguments: &Value, cwd: &Path) -> EaiResult<Value> {
    json_result(call_result(name, arguments, Some(cwd))?)
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn structured_results_keep_empty_text_and_tool_errors_fail_closed() {
        assert_eq!(
            json_result(json!({"structuredContent":{"text":""},"isError":false})).unwrap(),
            json!({"text":""})
        );
        assert_eq!(
            json_result(json!({"content":[{"type":"text","text":"[1,null]"}]})).unwrap(),
            json!([1, null])
        );
        for result in [
            json!({"structuredContent":{},"isError":true}),
            json!({"structuredContent":{},"isError":"false"}),
            json!({"structuredContent":[]}),
            json!({"content":[]}),
        ] {
            assert!(json_result(result).is_err());
        }
    }
}

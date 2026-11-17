//! Standard MCP handoff to the installed configuration authority owner.
//!
//! The owner performs registry selection and permission checks locally. A missing,
//! failed or malformed owner never proves that a configured provider is absent.

use crate::mcp_client::{ClientInfo, McpClient, StdioTransport};
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use std::path::{Path, PathBuf};
use std::process::Stdio;
use susi_error::{EaiError, EaiResult};

/// Public stdio MCP tool served by the configuration runtime.
pub const TOOL: &str = "susi_configuration_call_v1";
/// Identity retained by the storage guard's original registry sessions.
pub const CLIENT: ClientInfo = ClientInfo {
    name: "susi-storage-guard",
    version: env!("CARGO_PKG_VERSION"),
};

/// Lossless request for the workflow provider, retaining the caller's path semantics.
#[derive(Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Request {
    pub tool: String,
    pub arguments: Value,
    #[serde(default, with = "crate::native_path::option")]
    pub cwd: Option<PathBuf>,
}

#[derive(Serialize, Deserialize)]
#[serde(tag = "kind", content = "result", deny_unknown_fields)]
enum Reply {
    Absent,
    Present(Value),
}

/// Dispatch once through the installed authority owner. Only a healthy owner or
/// an empty, strictly decoded registry can report that no provider claims a tool.
pub fn try_call(name: &str, arguments: &Value, cwd: Option<&Path>) -> EaiResult<Option<Value>> {
    if name != crate::workflow_wire::WORKFLOW_TOOL || !arguments.is_object() {
        return Err(EaiError::config("invalid configuration bridge request"));
    }
    let registry =
        crate::mcp::read_mcp_config(&crate::mcp::mcp_config_path()).map_err(EaiError::config)?;
    if registry.mcp_servers.is_empty() {
        return Ok(None);
    }
    let program = crate::vault_exec::commercial_executable().ok_or_else(|| {
        EaiError::config(
            "configured MCP providers require Susi Commercial Apps; install it to serve `susi-commercial config-mcp-bridge`",
        )
    })?;
    let mut command = susi_paths::console::background_command(&program);
    command
        .arg("config-mcp-bridge")
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::inherit());
    let transport = StdioTransport::spawn(&mut command)?.with_timeout_secs(12);
    let mut client = McpClient::new(transport).with_client_info(CLIENT);
    let result = dispatch(&mut client, name, arguments, cwd);
    // A cleanup error cannot replay or invalidate an already completed operation.
    let _ = client.close();
    result
}

fn dispatch<T: crate::mcp_client::Transport>(
    client: &mut McpClient<T>,
    name: &str,
    arguments: &Value,
    cwd: Option<&Path>,
) -> EaiResult<Option<Value>> {
    let tools = client.read_tool_catalog(true)?;
    if tools.iter().filter(|tool| tool.name == TOOL).count() != 1 {
        return Err(EaiError::protocol(
            "configuration owner does not advertise its MCP bridge",
        ));
    }
    let request = serde_json::to_value(Request {
        tool: name.to_owned(),
        arguments: arguments.clone(),
        cwd: cwd.map(Path::to_path_buf),
    })
    .map_err(|error| EaiError::protocol(format!("configuration bridge request: {error}")))?;
    // The selected provider retains its existing operation-specific deadline;
    // only discovery and writes have a new-process transport deadline.
    client.set_call_timeout(None);
    let response = client.call_result(TOOL, &request)?;
    match crate::workflow_wire::decode_workflow_result(response)? {
        Reply::Absent => Ok(None),
        Reply::Present(value) => Ok(Some(value)),
    }
}

/// Standard result envelope, including the original typed refusal without loss.
pub fn result(value: EaiResult<Option<Value>>) -> Value {
    match value {
        Ok(value) => {
            let value = json!(value.map_or(Reply::Absent, Reply::Present));
            json!({"content":[{"type":"text","text":value.to_string()}],
                "structuredContent":value,"isError":false})
        }
        Err(error) => {
            let value = crate::workflow_wire::workflow_error_value(error);
            json!({"content":[{"type":"text","text":value.to_string()}],
                "structuredContent":value,"isError":true})
        }
    }
}

#[cfg(test)]
#[path = "tests/config_bridge.rs"]
mod tests;

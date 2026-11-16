//! Compatibility entry points for the shared authorized MCP protocol client.

use serde_json::Value;
use std::path::Path;
use susi_config::mcp_session;
use susi_error::EaiResult;

fn call_with_revision(
    name: &str,
    arguments: &Value,
    cwd: Option<&Path>,
) -> EaiResult<(Value, Option<String>)> {
    #[cfg(test)]
    if let Some(result) = fixture::dispatch(name, arguments, cwd) {
        return result;
    }
    // Existing harness library and standalone callers retain their HTTP adapter.
    crate::register_tool_backend();
    mcp_session::call_reply_with_info(
        name,
        arguments,
        cwd,
        susi_config::mcp_client::ClientInfo {
            name: "susi-harness",
            version: env!("CARGO_PKG_VERSION"),
        },
    )
    .map(|reply| (reply.result, reply.protocol_version))
}

pub(crate) fn call_result(name: &str, arguments: &Value, cwd: Option<&Path>) -> EaiResult<Value> {
    call_with_revision(name, arguments, cwd).map(|(result, _)| result)
}

pub(crate) fn call_json(name: &str, arguments: &Value) -> EaiResult<Value> {
    let (result, revision) = call_with_revision(name, arguments, None)?;
    mcp_session::json_result(result, revision.as_deref())
}

pub(crate) fn call_text_in(name: &str, arguments: &Value, cwd: &Path) -> EaiResult<String> {
    mcp_session::text_result(call_result(name, arguments, Some(cwd))?)
}

#[cfg(test)]
#[path = "tests/tool_backend_fixture.rs"]
pub(crate) mod fixture;

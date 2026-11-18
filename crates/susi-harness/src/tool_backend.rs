//! Compatibility entry points for the shared authorized MCP protocol client.

use serde_json::Value;
use std::path::Path;
use susi_config::mcp_session;
use susi_error::EaiResult;

fn client_info() -> susi_config::mcp_client::ClientInfo {
    crate::register_tool_backend();
    susi_config::mcp_client::ClientInfo {
        name: "susi-harness",
        version: env!("CARGO_PKG_VERSION"),
    }
}

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
    mcp_session::call_reply_with_info(name, arguments, cwd, client_info())
        .map(|reply| (reply.result, reply.protocol_version))
}

pub(crate) fn call_result(name: &str, arguments: &Value, cwd: Option<&Path>) -> EaiResult<Value> {
    call_with_revision(name, arguments, cwd).map(|(result, _)| result)
}

pub(crate) fn call_json(name: &str, arguments: &Value) -> EaiResult<Value> {
    let (result, revision) = call_with_revision(name, arguments, None)?;
    mcp_session::json_result(result, revision.as_deref())
}

pub(crate) fn try_call_result(
    name: &str,
    arguments: &Value,
    cwd: Option<&Path>,
) -> EaiResult<Option<Value>> {
    #[cfg(test)]
    if let Some(result) = fixture::dispatch(name, arguments, cwd) {
        return result.map(|(value, _)| Some(value));
    }
    Ok(
        mcp_session::try_call_reply_with_info(name, arguments, cwd, client_info())?
            .map(|reply| reply.result),
    )
}

pub(crate) fn try_call_text_in(
    name: &str,
    arguments: &Value,
    cwd: &Path,
) -> EaiResult<Option<String>> {
    try_call_result(name, arguments, Some(cwd))?
        .map(mcp_session::text_result)
        .transpose()
}

#[cfg(test)]
#[path = "tests/tool_backend_fixture.rs"]
pub(crate) mod fixture;

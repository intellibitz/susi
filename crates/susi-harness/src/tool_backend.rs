//! Resolve installed tools through the existing authorized MCP registry.

use crate::mcp::{HttpSocketTransport, McpClient, StdioTransport, Transport};
use serde_json::{json, Value};
use sha2::{Digest, Sha256};
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
    fn set_call_timeout(&mut self, timeout: Option<Duration>) {
        match self {
            Self::Stdio(peer) => peer.set_call_timeout(timeout),
            Self::Http(peer) => peer.set_call_timeout(timeout),
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
    call_with_revision(name, arguments, cwd).map(|(result, _)| result)
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
            let schema = tools
                .iter()
                .find(|tool| tool.name == name)
                .map(|tool| tool.schema.clone());
            let upload = tools.iter().any(|tool| tool.name == UPLOAD_TOOL);
            selected = Some((client, schema.unwrap_or(Value::Null), upload));
        }
    }
    let (mut client, schema, upload) = selected.ok_or_else(|| {
        EaiError::config(format!(
            "no available authorized MCP provider advertises '{name}' ({unavailable} unavailable)"
        ))
    })?;
    client.set_timeout(Duration::from_secs(60));
    let result = transfer_call(&mut client, name, arguments, &schema, upload);
    let revision = client.negotiated_protocol_version().map(str::to_owned);
    // Closing is cleanup, never a reason to replay a completed tool call.
    let _ = client.close();
    result.map(|result| (result, revision))
}

const UPLOAD_TOOL: &str = "susi_resource_upload";
const CHUNK_BYTES: usize = 256 * 1024;
const INLINE_BYTES: usize = 1024 * 1024;

fn digest(bytes: &[u8]) -> String {
    susi_paths::hex::encode(Sha256::digest(bytes))
}

fn transfer_error() -> EaiError {
    EaiError::protocol("invalid session-owned MCP resource transfer")
}

fn is_error(result: &Value) -> EaiResult<bool> {
    match result.get("isError") {
        None => Ok(false),
        Some(Value::Bool(flag)) => Ok(*flag),
        _ => Err(transfer_error()),
    }
}

fn resource_root(uri: &str, direction: &str) -> bool {
    uri.strip_prefix("susi-resource://")
        .and_then(|rest| rest.strip_suffix(&format!("/{direction}")))
        .is_some_and(|token| {
            token.len() == 64
                && token
                    .bytes()
                    .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
        })
}

fn upload_arguments<T: Transport>(
    client: &mut McpClient<T>,
    name: &str,
    text: &str,
) -> EaiResult<Value> {
    let hash = digest(text.as_bytes());
    let reply = client.call_result(
        UPLOAD_TOOL,
        &json!({"action":"begin","tool":name,"size":text.len(),"sha256":hash}),
    )?;
    let descriptor = json_result(reply, client.negotiated_protocol_version())?;
    let uri = descriptor
        .get("uri")
        .and_then(Value::as_str)
        .ok_or_else(transfer_error)?;
    if !resource_root(uri, "request")
        || descriptor["mimeType"] != "application/json"
        || descriptor["size"].as_u64() != u64::try_from(text.len()).ok()
        || descriptor["sha256"] != hash
    {
        return Err(transfer_error());
    }
    let mut offset = 0;
    while offset < text.len() {
        let mut end = (offset + CHUNK_BYTES).min(text.len());
        while !text.is_char_boundary(end) {
            end -= 1;
        }
        let reply = client.call_result(
            UPLOAD_TOOL,
            &json!({"action":"append","uri":uri,"offset":offset,"text":&text[offset..end]}),
        )?;
        let ack = json_result(reply, client.negotiated_protocol_version())?;
        if ack["received"].as_u64() != u64::try_from(end).ok() {
            return Err(transfer_error());
        }
        offset = end;
    }
    Ok(json!({"_resource":descriptor}))
}

fn resource_text<T: Transport>(
    client: &mut McpClient<T>,
    uri: &str,
    mime: &str,
) -> EaiResult<String> {
    let response = client.read_resource(uri)?;
    let contents = response
        .get("contents")
        .and_then(Value::as_array)
        .filter(|contents| contents.len() == 1)
        .ok_or_else(transfer_error)?;
    let entry = &contents[0];
    if entry["uri"] != uri || entry["mimeType"] != mime {
        return Err(transfer_error());
    }
    entry
        .get("text")
        .and_then(Value::as_str)
        .map(str::to_owned)
        .ok_or_else(transfer_error)
}

fn hydrate<T: Transport>(client: &mut McpClient<T>, result: Value) -> EaiResult<Value> {
    let Some(contents) = result.get("content").and_then(Value::as_array) else {
        return Ok(result);
    };
    if contents.len() != 1 {
        return Ok(result);
    }
    let block = &contents[0];
    let (uri, manifest_text) = match block["type"].as_str() {
        Some("resource_link") if block["_meta"]["susi/resource"] == "json-chunks-v1" => {
            let uri = block
                .get("uri")
                .and_then(Value::as_str)
                .ok_or_else(transfer_error)?;
            if !resource_root(uri, "result")
                || block["mimeType"] != "application/json"
                || block["_meta"]["susi/content"] != "mcp-tool-result"
            {
                return Err(transfer_error());
            }
            let text = resource_text(client, uri, "application/json")?;
            if block["size"].as_u64() != u64::try_from(text.len()).ok() {
                return Err(transfer_error());
            }
            (uri, text)
        }
        Some("resource") => {
            let resource = &block["resource"];
            let Some(uri) = resource
                .get("uri")
                .and_then(Value::as_str)
                .filter(|uri| resource_root(uri, "result"))
            else {
                return Ok(result);
            };
            if resource["mimeType"] != "application/json" {
                return Err(transfer_error());
            }
            let text = resource
                .get("text")
                .and_then(Value::as_str)
                .ok_or_else(transfer_error)?;
            (uri, text.to_owned())
        }
        _ => return Ok(result),
    };
    let manifest: Value = serde_json::from_str(&manifest_text).map_err(|_| transfer_error())?;
    let size = manifest["size"].as_u64().ok_or_else(transfer_error)?;
    let chunks = manifest["chunks"].as_u64().ok_or_else(transfer_error)?;
    if manifest["format"] != "susi-json-chunks-v1"
        || manifest["mimeType"] != "application/json"
        || manifest["chunkUriTemplate"] != format!("{uri}/{{index}}")
        || chunks == 0
        || chunks > size
    {
        return Err(transfer_error());
    }
    let mut text = String::new();
    for index in 0..chunks {
        let chunk = resource_text(client, &format!("{uri}/{index}"), "text/plain")?;
        if chunk.is_empty() || chunk.len() > CHUNK_BYTES {
            return Err(transfer_error());
        }
        text.push_str(&chunk);
        if u64::try_from(text.len()).map_err(|_| transfer_error())? > size {
            return Err(transfer_error());
        }
    }
    if u64::try_from(text.len()).ok() != Some(size) || manifest["sha256"] != digest(text.as_bytes())
    {
        return Err(transfer_error());
    }
    let hydrated: Value =
        susi_config::protocol_json::parse(text.as_bytes()).map_err(|_| transfer_error())?;
    if !hydrated.is_object()
        || is_error(&hydrated)? != is_error(&result)?
        || hydrated
            .get("content")
            .and_then(Value::as_array)
            .is_some_and(|blocks| {
                blocks
                    .iter()
                    .any(|block| block["_meta"]["susi/resource"] == "json-chunks-v1")
            })
    {
        return Err(transfer_error());
    }
    let _ = client.call_result(UPLOAD_TOOL, &json!({"action":"discard","uri":uri}));
    Ok(hydrated)
}

fn transfer_call<T: Transport>(
    client: &mut McpClient<T>,
    name: &str,
    arguments: &Value,
    schema: &Value,
    upload: bool,
) -> EaiResult<Value> {
    let text = serde_json::to_string(arguments).map_err(|_| transfer_error())?;
    let resource_schema = schema
        .get("oneOf")
        .and_then(Value::as_array)
        .is_some_and(|branches| {
            branches.iter().any(|branch| {
                branch
                    .get("properties")
                    .and_then(|properties| properties.get("_resource"))
                    .is_some()
            })
        });
    let staged = if text.len() > INLINE_BYTES && upload && resource_schema {
        Some(upload_arguments(client, name, &text)?)
    } else {
        None
    };
    client.set_call_timeout(None);
    let result = client.call_result(name, staged.as_ref().unwrap_or(arguments));
    client.set_call_timeout(Some(Duration::from_secs(60)));
    hydrate(client, result?)
}

fn json_result(result: Value, revision: Option<&str>) -> EaiResult<Value> {
    if result
        .get("isError")
        .is_some_and(|flag| flag != &Value::Bool(false))
    {
        return Err(EaiError::internal("MCP provider returned a tool error"));
    }
    if let Some(structured) = result.get("structuredContent") {
        if !structured.is_object() && revision != Some("2026-07-28") {
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
    susi_config::protocol_json::parse(text.as_bytes())
        .map_err(|error| EaiError::internal(format!("MCP tool JSON: {error}")))
}

/// Read structured content from a registered, authorized standard MCP tool.
pub(crate) fn call_json(name: &str, arguments: &Value) -> EaiResult<Value> {
    let (result, revision) = call_with_revision(name, arguments, None)?;
    json_result(result, revision.as_deref())
}

/// Return one native text block without changing empty or JSON-looking text.
pub(crate) fn call_text_in(name: &str, arguments: &Value, cwd: &Path) -> EaiResult<String> {
    let result = call_result(name, arguments, Some(cwd))?;
    if result
        .get("isError")
        .is_some_and(|flag| flag != &Value::Bool(false))
    {
        return Err(EaiError::internal("MCP provider returned a tool error"));
    }
    let blocks = result
        .get("content")
        .and_then(Value::as_array)
        .filter(|blocks| blocks.len() == 1)
        .ok_or_else(transfer_error)?;
    if blocks[0]["type"] != "text" {
        return Err(transfer_error());
    }
    blocks[0]
        .get("text")
        .and_then(Value::as_str)
        .map(str::to_owned)
        .ok_or_else(transfer_error)
}

#[cfg(test)]
#[path = "tests/tool_backend.rs"]
mod tests;

#[cfg(test)]
#[path = "tests/tool_backend_fixture.rs"]
pub(crate) mod fixture;

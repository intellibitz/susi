//! Authorized MCP discovery, owned sessions, and session-bound resource transfer.

use crate::mac_policy::MacPolicy;
use crate::mcp::{self, McpServerConfig};
use crate::mcp_client::{ClientInfo, McpClient, StdioTransport, Transport};
use serde_json::{json, Value};
use sha2::{Digest, Sha256};
use std::path::{Path, PathBuf};
use std::sync::OnceLock;
use std::time::Duration;
use susi_error::{EaiError, EaiResult};

/// A complete tool result and the revision negotiated by its owning session.
pub struct ToolReply {
    pub result: Value,
    pub protocol_version: Option<String>,
}

/// A tool and provider selected once. Calls recheck current authority; a lost
/// connection is an error, never a reason to reconnect or replay an operation.
pub trait ToolSession: Send {
    fn call(&mut self, arguments: &Value) -> EaiResult<ToolReply>;
    fn close(&mut self) -> EaiResult<()>;
}

/// Application composition supplies its existing HTTP transport without adding
/// an HTTP stack to this protocol leaf. Stdio needs no external constructor.
pub type HttpTransport = fn(&McpServerConfig) -> EaiResult<Box<dyn Transport>>;
static HTTP_TRANSPORT: OnceLock<HttpTransport> = OnceLock::new();

pub fn register_http_transport(connect: HttpTransport) {
    HTTP_TRANSPORT.get_or_init(|| connect);
}

impl Transport for Box<dyn Transport> {
    fn exchange(&mut self, request: &Value) -> EaiResult<Value> {
        self.as_mut().exchange(request)
    }
    fn notify(&mut self, request: &Value) -> EaiResult<()> {
        self.as_mut().notify(request)
    }
    fn set_protocol_version(&mut self, version: &str) {
        self.as_mut().set_protocol_version(version);
    }
    fn set_timeout(&mut self, timeout: Duration) {
        self.as_mut().set_timeout(timeout);
    }
    fn set_call_timeout(&mut self, timeout: Option<Duration>) {
        self.as_mut().set_call_timeout(timeout);
    }
    fn close(&mut self) -> EaiResult<()> {
        self.as_mut().close()
    }
}

fn connect(
    server: &McpServerConfig,
    cwd: Option<&Path>,
    client_info: ClientInfo,
) -> EaiResult<McpClient<Box<dyn Transport>>> {
    let policy = MacPolicy::global();
    mcp::authorize_mcp_transport(server, policy).map_err(EaiError::config)?;
    let channel: Box<dyn Transport> = if server.http_scope().map_err(EaiError::config)?.is_some() {
        if cwd.is_some() {
            return Err(EaiError::config(
                "a process working directory requires a stdio MCP provider",
            ));
        }
        HTTP_TRANSPORT
            .get()
            .ok_or_else(|| EaiError::config("MCP HTTP transport is not registered"))?(
            server
        )?
    } else {
        let mut command =
            mcp::prepare_authorized_stdio_command(server, policy).map_err(EaiError::config)?;
        if let Some(cwd) = cwd {
            command.current_dir(cwd);
        }
        let child = command
            .spawn()
            .map_err(|error| EaiError::io(format!("MCP provider spawn: {error}")))?;
        Box::new(StdioTransport::from_child(child)?.with_timeout_secs(12))
    };
    Ok(McpClient::new(channel).with_client_info(client_info))
}

pub fn call_reply(name: &str, arguments: &Value, cwd: Option<&Path>) -> EaiResult<ToolReply> {
    call_reply_with_info(name, arguments, cwd, ClientInfo::default())
}

/// Call once with the application identity retained by a compatibility adapter.
pub fn call_reply_with_info(
    name: &str,
    arguments: &Value,
    cwd: Option<&Path>,
    client_info: ClientInfo,
) -> EaiResult<ToolReply> {
    if !arguments.is_object() {
        return Err(EaiError::config("MCP object arguments are required"));
    }
    call_session(open_tool_with_info(name, cwd, client_info)?, arguments)
}

/// Return `None` only after healthy discovery declares no matching tool.
pub fn try_call_reply_with_info(
    name: &str,
    arguments: &Value,
    cwd: Option<&Path>,
    client_info: ClientInfo,
) -> EaiResult<Option<ToolReply>> {
    if !arguments.is_object() {
        return Err(EaiError::config("MCP object arguments are required"));
    }
    find_tool(name, cwd, client_info, true)?
        .map(|session| call_session(session, arguments))
        .transpose()
}

fn call_session(mut session: Box<dyn ToolSession>, arguments: &Value) -> EaiResult<ToolReply> {
    let reply = session.call(arguments);
    // Cleanup cannot invalidate or replay an already completed operation.
    let _ = session.close();
    reply
}

/// Preserve the complete MCP result, including typed provider error content.
pub fn call_result(name: &str, arguments: &Value, cwd: Option<&Path>) -> EaiResult<Value> {
    call_reply(name, arguments, cwd).map(|reply| reply.result)
}

/// Decode an unambiguous JSON success result from a registered MCP tool.
pub fn call_json(name: &str, arguments: &Value, cwd: Option<&Path>) -> EaiResult<Value> {
    let reply = call_reply(name, arguments, cwd)?;
    json_result(reply.result, reply.protocol_version.as_deref())
}

struct Session {
    name: String,
    registry: PathBuf,
    server_name: String,
    server: McpServerConfig,
    schema: Value,
    upload: bool,
    client: Option<McpClient<Box<dyn Transport>>>,
}

impl Session {
    fn authorize(&self, policy: &MacPolicy) -> EaiResult<()> {
        let registry = mcp::read_mcp_config(&self.registry).map_err(EaiError::config)?;
        let current = registry
            .mcp_servers
            .get(&self.server_name)
            .ok_or_else(|| EaiError::config("the selected MCP provider is no longer registered"))?;
        let encode = |server: &McpServerConfig| {
            serde_json::to_value(server).map_err(|error| EaiError::config(error.to_string()))
        };
        if encode(current)? != encode(&self.server)? {
            return Err(EaiError::config(
                "the selected MCP provider configuration changed",
            ));
        }
        mcp::authorize_mcp_transport(current, policy).map_err(EaiError::config)
    }

    fn call_with_policy(&mut self, arguments: &Value, policy: &MacPolicy) -> EaiResult<ToolReply> {
        let result = (|| {
            if !arguments.is_object() {
                return Err(EaiError::config("MCP object arguments are required"));
            }
            self.authorize(policy)?;
            let client = self
                .client
                .as_mut()
                .ok_or_else(|| EaiError::protocol("the MCP tool session is closed"))?;
            client.set_timeout(Duration::from_secs(60));
            let result = transfer_call(client, &self.name, arguments, &self.schema, self.upload)?;
            Ok(ToolReply {
                result,
                protocol_version: client.negotiated_protocol_version().map(str::to_owned),
            })
        })();
        if result.is_err() {
            let _ = self.close();
        }
        result
    }
}

impl ToolSession for Session {
    fn call(&mut self, arguments: &Value) -> EaiResult<ToolReply> {
        self.call_with_policy(arguments, MacPolicy::global())
    }

    fn close(&mut self) -> EaiResult<()> {
        self.client
            .take()
            .map_or(Ok(()), |mut client| client.close())
    }
}

pub fn open_tool(name: &str, cwd: Option<&Path>) -> EaiResult<Box<dyn ToolSession>> {
    open_tool_with_info(name, cwd, ClientInfo::default())
}

pub fn open_tool_with_info(
    name: &str,
    cwd: Option<&Path>,
    client_info: ClientInfo,
) -> EaiResult<Box<dyn ToolSession>> {
    find_tool(name, cwd, client_info, false)?.ok_or_else(|| no_provider(name, 0))
}

fn no_provider(name: &str, unavailable: usize) -> EaiError {
    EaiError::config(format!(
        "no available authorized MCP provider advertises '{name}' ({unavailable} unavailable)"
    ))
}

fn find_tool(
    name: &str,
    cwd: Option<&Path>,
    client_info: ClientInfo,
    inspect_remote: bool,
) -> EaiResult<Option<Box<dyn ToolSession>>> {
    if name.is_empty() {
        return Err(EaiError::config("MCP tool name is required"));
    }
    let registry_path = mcp::mcp_config_path();
    let registry = mcp::read_mcp_config(&registry_path).map_err(EaiError::config)?;
    let mut servers: Vec<_> = registry.mcp_servers.into_iter().collect();
    servers.sort_by(|left, right| left.0.cmp(&right.0));
    let mut selected = None;
    let mut unavailable = 0usize;
    for (server_name, server) in servers {
        let remote_cwd =
            inspect_remote && cwd.is_some() && matches!(server.http_scope(), Ok(Some(_)));
        let transport_cwd = if remote_cwd { None } else { cwd };
        let discovery = connect(&server, transport_cwd, client_info).and_then(|mut client| {
            let tools = client.list_tools()?;
            Ok((client, tools))
        });
        let Ok((client, tools)) = discovery else {
            unavailable += 1;
            continue;
        };
        let mut matches = tools.iter().filter(|tool| tool.name == name);
        let Some(tool) = matches.next() else { continue };
        if remote_cwd {
            unavailable += 1;
            continue;
        }
        if matches.next().is_some() || selected.is_some() {
            return Err(EaiError::config(format!(
                "MCP tool '{name}' has ambiguous providers"
            )));
        }
        selected = Some(Session {
            name: name.to_owned(),
            registry: registry_path.clone(),
            server_name,
            server,
            schema: tool.schema.clone(),
            upload: tools.iter().any(|tool| tool.name == UPLOAD_TOOL),
            client: Some(client),
        });
    }
    match selected {
        Some(session) => Ok(Some(Box::new(session))),
        None if unavailable == 0 => Ok(None),
        None => Err(no_provider(name, unavailable)),
    }
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
        crate::protocol_json::parse(text.as_bytes()).map_err(|_| transfer_error())?;
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

/// Interpret JSON content using the session's negotiated protocol revision.
pub fn json_result(result: Value, revision: Option<&str>) -> EaiResult<Value> {
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
    crate::protocol_json::parse(text.as_bytes())
        .map_err(|error| EaiError::internal(format!("MCP tool JSON: {error}")))
}

/// Return one text block without changing empty or JSON-looking text.
pub fn text_result(result: Value) -> EaiResult<String> {
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
#[path = "tests/mcp_session.rs"]
mod tests;

//! The MCP client: tools that live outside the binary.
//!
//! MCP is the tool protocol of this harness, the way OpenAI-compatible chat is
//! the model protocol. A client speaks JSON-RPC 2.0 over a transport; the
//! transport is a trait so the protocol is testable without a process, and
//! [`StdioTransport`] is the production one for a child server.
//!
//! The client does the minimum a tool caller needs: `initialize`, `tools/list`
//! and `tools/call`, with request ids and error surfacing.

use crate::tools::{Policy, ToolSpec};
use serde_json::{json, Value};
use std::io::{BufRead, BufReader, Write};
use std::process::{Child, ChildStdin, ChildStdout, Command, Stdio};
use susi_error::{EaiError, EaiResult};

/// The protocol revision this client speaks.
pub const PROTOCOL_VERSION: &str = "2024-11-05";

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

/// A JSON-RPC channel to one MCP server.
pub trait Transport: Send {
    /// Send one request and read its response.
    ///
    /// # Errors
    ///
    /// Returns a transport error when the server cannot be reached, the stream
    /// closes, or the response is not JSON.
    fn exchange(&mut self, request: &Value) -> EaiResult<Value>;

    /// Send a notification (no id, no response).
    ///
    /// # Errors
    ///
    /// Returns a transport error when the notification cannot be written.
    fn notify(&mut self, _notification: &Value) -> EaiResult<()> {
        Ok(())
    }
}

/// A client for one MCP server.
/// Upper bound on `tools/list` pages one listing follows.
pub const MAX_LIST_PAGES: usize = 50;

#[derive(Debug)]
pub struct McpClient<T: Transport> {
    transport: T,
    next_id: u64,
    initialized: bool,
}

impl<T: Transport> McpClient<T> {
    /// A client over an already-connected transport.
    pub fn new(transport: T) -> Self {
        Self {
            transport,
            next_id: 0,
            initialized: false,
        }
    }

    /// Perform the MCP handshake once.
    ///
    /// # Errors
    ///
    /// Returns the server's error or a transport error.
    pub fn initialize(&mut self) -> EaiResult<Value> {
        if self.initialized {
            return Ok(json!({}));
        }
        let result = self.request(
            "initialize",
            json!({
                "protocolVersion": PROTOCOL_VERSION,
                "capabilities": {},
                "clientInfo": {"name": "susi-harness", "version": env!("CARGO_PKG_VERSION")}
            }),
        )?;
        self.transport
            .notify(&json!({"jsonrpc": "2.0", "method": "notifications/initialized"}))?;
        self.initialized = true;
        Ok(result)
    }

    /// The tools the server advertises, across every `tools/list` page.
    ///
    /// The listing follows `nextCursor` until the server omits it, bounded by
    /// [`MAX_LIST_PAGES`] so a misbehaving server cannot loop the client.
    ///
    /// # Errors
    ///
    /// Returns an error when the handshake or any page of the listing fails,
    /// when a page carries a non-string cursor, or when the server keeps
    /// paging past the bound.
    pub fn list_tools(&mut self) -> EaiResult<Vec<McpTool>> {
        self.initialize()?;
        let mut tools = Vec::new();
        let mut cursor: Option<String> = None;
        for _ in 0..MAX_LIST_PAGES {
            let params = cursor
                .as_ref()
                .map_or_else(|| json!({}), |cursor| json!({"cursor": cursor}));
            let result = self.request("tools/list", params)?;
            let page = result
                .get("tools")
                .and_then(Value::as_array)
                .ok_or_else(|| EaiError::internal("mcp tools/list returned no tools array"))?;
            tools.extend(page.iter().cloned());
            cursor = match result.get("nextCursor") {
                None | Some(Value::Null) => return Ok(Self::parse_tools(&tools)),
                Some(Value::String(next)) => Some(next.clone()),
                Some(_) => {
                    return Err(EaiError::internal(
                        "mcp tools/list returned a non-string nextCursor",
                    ))
                }
            };
        }
        Err(EaiError::internal(format!(
            "mcp tools/list still paging after {MAX_LIST_PAGES} pages"
        )))
    }

    fn parse_tools(tools: &[Value]) -> Vec<McpTool> {
        tools
            .iter()
            .filter_map(|tool| {
                let name = tool.get("name").and_then(Value::as_str)?;
                Some(McpTool {
                    name: name.to_string(),
                    description: tool
                        .get("description")
                        .and_then(Value::as_str)
                        .unwrap_or_default()
                        .to_string(),
                    schema: tool
                        .get("inputSchema")
                        .cloned()
                        .unwrap_or_else(|| json!({"type": "object"})),
                })
            })
            .collect()
    }

    /// Call one tool and return its text content.
    ///
    /// Non-text content is returned as its JSON form so a caller never gets an
    /// empty answer for a tool that answered.
    ///
    /// # Errors
    ///
    /// Returns the server's error or a transport error.
    pub fn call(&mut self, name: &str, arguments: &Value) -> EaiResult<String> {
        self.initialize()?;
        let result = self.request("tools/call", json!({"name": name, "arguments": arguments}))?;
        let mut text = String::new();
        if let Some(blocks) = result.get("content").and_then(Value::as_array) {
            for block in blocks {
                match block.get("type").and_then(Value::as_str) {
                    Some("text") => {
                        if let Some(chunk) = block.get("text").and_then(Value::as_str) {
                            if !text.is_empty() {
                                text.push('\n');
                            }
                            text.push_str(chunk);
                        }
                    }
                    Some(_) => {
                        if !text.is_empty() {
                            text.push('\n');
                        }
                        text.push_str(&block.to_string());
                    }
                    None => {}
                }
            }
        }
        if text.is_empty() {
            Ok(result.to_string())
        } else {
            Ok(text)
        }
    }

    fn request(&mut self, method: &str, params: Value) -> EaiResult<Value> {
        self.next_id = self.next_id.saturating_add(1);
        let id = self.next_id;
        let request = json!({
            "jsonrpc": "2.0",
            "id": id,
            "method": method,
            "params": params,
        });
        let response = self.transport.exchange(&request)?;
        if let Some(error) = response.get("error") {
            let message = error
                .get("message")
                .and_then(Value::as_str)
                .unwrap_or("unknown error");
            return Err(EaiError::internal(format!("mcp {method}: {message}")));
        }
        response
            .get("result")
            .cloned()
            .ok_or_else(|| EaiError::internal(format!("mcp {method}: response has no result")))
    }
}

/// A JSON-RPC transport over a child process's stdio.
#[derive(Debug)]
pub struct StdioTransport {
    child: Child,
    stdin: ChildStdin,
    stdout: BufReader<ChildStdout>,
}

impl StdioTransport {
    /// Spawn an MCP server and connect to it.
    ///
    /// # Errors
    ///
    /// Returns an I/O error when the process cannot be spawned or its pipes
    /// cannot be taken.
    pub fn spawn(program: &str, args: &[String]) -> EaiResult<Self> {
        let mut child = Command::new(program)
            .args(args)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::null())
            .spawn()
            .map_err(|error| EaiError::io(format!("mcp spawn {program}: {error}")))?;
        let stdin = child
            .stdin
            .take()
            .ok_or_else(|| EaiError::internal("mcp: child has no stdin"))?;
        let stdout = child
            .stdout
            .take()
            .ok_or_else(|| EaiError::internal("mcp: child has no stdout"))?;
        Ok(Self {
            child,
            stdin,
            stdout: BufReader::new(stdout),
        })
    }
}

impl Transport for StdioTransport {
    fn exchange(&mut self, request: &Value) -> EaiResult<Value> {
        let line = serde_json::to_string(request)
            .map_err(|error| EaiError::internal(format!("mcp request: {error}")))?;
        writeln!(self.stdin, "{line}")
            .map_err(|error| EaiError::io(format!("mcp write: {error}")))?;
        self.stdin
            .flush()
            .map_err(|error| EaiError::io(format!("mcp flush: {error}")))?;
        let mut response = String::new();
        self.stdout
            .read_line(&mut response)
            .map_err(|error| EaiError::io(format!("mcp read: {error}")))?;
        if response.trim().is_empty() {
            return Err(EaiError::io("mcp server closed the stream"));
        }
        serde_json::from_str(&response)
            .map_err(|error| EaiError::internal(format!("mcp response: {error}")))
    }

    fn notify(&mut self, notification: &Value) -> EaiResult<()> {
        let line = serde_json::to_string(notification)
            .map_err(|error| EaiError::internal(format!("mcp notification: {error}")))?;
        writeln!(self.stdin, "{line}")
            .map_err(|error| EaiError::io(format!("mcp write: {error}")))?;
        self.stdin
            .flush()
            .map_err(|error| EaiError::io(format!("mcp flush: {error}")))
    }
}

impl Drop for StdioTransport {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::VecDeque;

    struct Scripted {
        responses: VecDeque<Value>,
        requests: Vec<Value>,
    }

    impl Scripted {
        fn new(responses: Vec<Value>) -> Self {
            Self {
                responses: responses.into(),
                requests: Vec::new(),
            }
        }
    }

    impl Transport for Scripted {
        fn exchange(&mut self, request: &Value) -> EaiResult<Value> {
            self.requests.push(request.clone());
            self.responses
                .pop_front()
                .ok_or_else(|| EaiError::internal("scripted transport exhausted"))
        }
    }

    fn initialized() -> Value {
        json!({"result": {"protocolVersion": PROTOCOL_VERSION, "capabilities": {}}})
    }

    #[test]
    fn mcp_lists_tools_after_initialize() {
        let transport = Scripted::new(vec![
            initialized(),
            json!({"result": {"tools": [
                {"name": "search", "description": "Search", "inputSchema": {"type": "object"}},
                {"name": "fetch", "description": "Fetch", "inputSchema": {"type": "object"}}
            ]}}),
        ]);
        let mut client = McpClient::new(transport);
        let tools = client.list_tools().unwrap();
        assert_eq!(tools.len(), 2);
        assert_eq!(tools[0].name, "search");
        assert_eq!(client.transport.requests[0]["method"], "initialize");
        assert_eq!(client.transport.requests[1]["method"], "tools/list");
    }

    #[test]
    fn mcp_list_tools_follows_next_cursor_until_absent() {
        let transport = Scripted::new(vec![
            initialized(),
            json!({"result": {"tools": [{"name": "a"}], "nextCursor": "rev:1"}}),
            json!({"result": {"tools": [{"name": "b"}], "nextCursor": "rev:2"}}),
            json!({"result": {"tools": [{"name": "c"}]}}),
        ]);
        let mut client = McpClient::new(transport);
        let names: Vec<String> = client
            .list_tools()
            .unwrap()
            .into_iter()
            .map(|tool| tool.name)
            .collect();
        assert_eq!(names, ["a", "b", "c"]);
        let requests = &client.transport.requests;
        assert_eq!(requests.len(), 4);
        assert!(requests[1]["params"].get("cursor").is_none());
        assert_eq!(requests[2]["params"]["cursor"], "rev:1");
        assert_eq!(requests[3]["params"]["cursor"], "rev:2");
    }

    #[test]
    fn mcp_list_tools_surfaces_a_cursor_error_and_bounds_paging() {
        let transport = Scripted::new(vec![
            initialized(),
            json!({"result": {"tools": [{"name": "a"}], "nextCursor": "rev:1"}}),
            json!({"error": {"code": -32602, "message": "Invalid cursor"}}),
        ]);
        let mut client = McpClient::new(transport);
        let error = client.list_tools().unwrap_err().to_string();
        assert!(error.contains("Invalid cursor"), "{error}");

        let mut endless = vec![initialized()];
        endless.extend(
            (0..=MAX_LIST_PAGES)
                .map(|n| json!({"result": {"tools": [], "nextCursor": format!("rev:{n}")}})),
        );
        let mut client = McpClient::new(Scripted::new(endless));
        let error = client.list_tools().unwrap_err().to_string();
        assert!(error.contains("still paging"), "{error}");
    }

    #[test]
    fn mcp_calls_a_tool_and_joins_text_blocks() {
        let transport = Scripted::new(vec![
            initialized(),
            json!({"result": {"content": [
                {"type": "text", "text": "first"},
                {"type": "text", "text": "second"}
            ]}}),
        ]);
        let mut client = McpClient::new(transport);
        let text = client.call("search", &json!({"q": "rust"})).unwrap();
        assert_eq!(text, "first\nsecond");
        let call = &client.transport.requests[1];
        assert_eq!(call["method"], "tools/call");
        assert_eq!(call["params"]["name"], "search");
        assert_eq!(call["params"]["arguments"]["q"], "rust");
    }

    #[test]
    fn mcp_surfaces_a_json_rpc_error() {
        let transport = Scripted::new(vec![
            initialized(),
            json!({"error": {"code": -32601, "message": "method not found"}}),
        ]);
        let mut client = McpClient::new(transport);
        let error = client.list_tools().unwrap_err().to_string();
        assert!(error.contains("method not found"), "{error}");
    }

    #[test]
    fn mcp_tools_turn_into_registry_specs() {
        let tool = McpTool {
            name: "search".into(),
            description: "Search".into(),
            schema: json!({"type": "object"}),
        };
        let spec = tool.to_spec(Policy::Ask);
        assert_eq!(spec.name, "search");
        assert_eq!(spec.policy, Policy::Ask);
    }
}

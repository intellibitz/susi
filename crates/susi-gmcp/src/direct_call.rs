//! Direct CLI and in-process execution for registered MCP tools.
//!
//! Provides parameter schema validation against registered/standard tool input schemas,
//! evidence recording via `EvidenceSession::capture_call`, and response formatting
//! adhering to the standard MCP `CallToolResult` JSON specification.

use crate::protocol::is_tool_result_error;
use crate::tool_registry::{GmcpClient, ToolRegistry};
use dashmap::DashMap;
use serde_json::{json, Value};
use std::path::Path;
use std::sync::OnceLock;
use susi_error::{EaiError, EaiResult};

static CUSTOM_SCHEMAS: OnceLock<DashMap<String, Value>> = OnceLock::new();

fn custom_schemas() -> &'static DashMap<String, Value> {
    CUSTOM_SCHEMAS.get_or_init(DashMap::new)
}

/// Register a custom JSON schema for an MCP tool parameter validation.
pub fn register_tool_schema(server: &str, tool: &str, schema: Value) {
    let key = if server.is_empty() || server == "susi" || server == "native" || server == "gmcp" {
        tool.to_string()
    } else {
        format!("{server}:{tool}")
    };
    custom_schemas().insert(key, schema);
}

/// Retrieve the effective input schema for a given tool.
pub fn get_tool_schema(server: &str, tool: &str) -> Value {
    let key = format!("{server}:{tool}");
    if let Some(entry) = custom_schemas().get(&key) {
        return entry.value().clone();
    }
    if let Some(entry) = custom_schemas().get(tool) {
        return entry.value().clone();
    }

    // Built-in schemas for common core tools
    match tool {
        "read_file" => json!({
            "type": "object",
            "properties": {
                "path": { "type": "string" }
            },
            "required": ["path"]
        }),
        "write_file" => json!({
            "type": "object",
            "properties": {
                "path": { "type": "string" },
                "content": { "type": "string" }
            },
            "required": ["path", "content"]
        }),
        "exec_command" => json!({
            "type": "object",
            "properties": {
                "command": { "type": "string" }
            },
            "required": ["command"]
        }),
        "tasks_pause" => json!({
            "type": "object",
            "properties": {
                "task_id": { "type": "string" }
            },
            "required": ["task_id"]
        }),
        "tasks_resume" => json!({
            "type": "object",
            "properties": {
                "task_id": { "type": "string" }
            },
            "required": ["task_id"]
        }),
        "os_ps" => json!({
            "type": "object",
            "properties": {
                "limit": { "type": "integer" }
            }
        }),
        "status" | "identity" | "list_models" | "os_sysinfo" => json!({
            "type": "object"
        }),
        _ => json!({
            "type": "object"
        }),
    }
}

/// Validate tool arguments against the tool's input schema.
pub fn validate_tool_args(server: &str, tool: &str, args: &Value) -> EaiResult<()> {
    if !args.is_object() {
        return Err(EaiError::protocol("Arguments must be a valid JSON object"));
    }

    let schema = get_tool_schema(server, tool);
    let validator = jsonschema::draft202012::options()
        .with_pattern_options(
            jsonschema::PatternOptions::fancy_regex()
                .backtrack_limit(20_000)
                .size_limit(1_000_000),
        )
        .build(&schema)
        .map_err(|e| EaiError::protocol(format!("Invalid input schema: {e}")))?;

    if !validator.is_valid(args) {
        let mut errors = Vec::new();
        for err in validator.iter_errors(args) {
            errors.push(format!("{}: {}", err.instance_path(), err));
        }
        let detail = if errors.is_empty() {
            "Arguments do not conform to schema".to_string()
        } else {
            errors.join("; ")
        };
        return Err(EaiError::protocol(format!(
            "Arguments do not match inputSchema for tool '{tool}': {detail}"
        )));
    }

    Ok(())
}

/// Format raw text or error output into standard MCP CallToolResult format.
pub fn format_tool_response(output: &str, is_error: bool) -> Value {
    json!({
        "content": [
            {
                "type": "text",
                "text": output
            }
        ],
        "isError": is_error
    })
}

pub type ToolHandlerFn = std::sync::Arc<dyn Fn(&Value, &Path) -> EaiResult<String> + Send + Sync>;
static CUSTOM_HANDLERS: OnceLock<DashMap<String, ToolHandlerFn>> = OnceLock::new();

fn custom_handlers() -> &'static DashMap<String, ToolHandlerFn> {
    CUSTOM_HANDLERS.get_or_init(DashMap::new)
}

/// Register a custom handler function for a tool invocation.
pub fn register_tool_handler(server: &str, tool: &str, handler: ToolHandlerFn) {
    let key = if server.is_empty() || server == "susi" || server == "native" || server == "gmcp" {
        tool.to_string()
    } else {
        format!("{server}:{tool}")
    };
    custom_handlers().insert(key, handler);
}

fn dispatch_native(tool: &str, args: &Value, workspace: &Path) -> EaiResult<String> {
    if let Some(entry) = custom_handlers().get(tool) {
        return (entry.value())(args, workspace);
    }

    match tool {
        "status" => crate::tools::CoreTools::status(args, workspace),
        "identity" => crate::tools::CoreTools::identity(args, workspace),
        "list_models" => crate::tools::CoreTools::list_models(args, workspace),
        "read_file" => crate::tools::CoreTools::read_file(args, workspace),
        "write_file" => crate::tools::CoreTools::write_file(args, workspace),
        "exec_command" => crate::tools::CoreTools::exec_command(args, workspace),
        "tasks_pause" => crate::tools::CoreTools::tasks_pause(args, workspace),
        "tasks_resume" => crate::tools::CoreTools::tasks_resume(args, workspace),
        "sovereign_dashboard" => crate::tools::CoreTools::sovereign_dashboard(args, workspace),
        "os_ps" => crate::tools::CoreTools::os_ps(args, workspace),
        "os_sysinfo" => crate::tools::CoreTools::os_sysinfo(args, workspace),
        _ => {
            let out = ToolRegistry::execute_tool(tool, args, workspace);
            if is_tool_result_error(&out) {
                Err(EaiError::protocol(out))
            } else {
                Ok(out)
            }
        }
    }
}

/// Directly invoke an MCP tool locally or via dynamic server proxy with parameter
/// schema validation, evidence ledger recording, and MCP-compliant response formatting.
pub fn call_tool_direct(
    server: &str,
    tool: &str,
    args: &Value,
    workspace: &Path,
) -> EaiResult<Value> {
    let call_id = if server.is_empty()
        || server == "susi"
        || server == "native"
        || server == "gmcp"
        || server == "local"
        || server == "builtin"
        || server == "core"
    {
        tool.to_string()
    } else {
        format!("{server}:{tool}")
    };

    // 1. Parameter Schema Validation
    if let Err(e) = validate_tool_args(server, tool, args) {
        let err_msg = e.to_string();
        // Record validation failure in evidence session if one exists or is created
        let _ =
            susi_core::capture::EvidenceSession::capture_call(&call_id, args, workspace, || {
                Err(EaiError::protocol(&err_msg))
            });
        return Ok(format_tool_response(&err_msg, true));
    }

    // 2. Ensure an active EvidenceSession for CLI/standalone callers
    let _activation = if susi_core::capture::EvidenceSession::for_workspace(workspace).is_none() {
        susi_core::capture::EvidenceSession::new(&format!("cli:mcp:{call_id}"), workspace, |s| {
            s.to_string()
        })
        .ok()
        .map(|sess| susi_core::capture::EvidenceSession::activate(&sess))
    } else {
        None
    };

    // 3. Execution Dispatch with Evidence Recording
    let is_native = server == "susi"
        || server == "native"
        || server == "gmcp"
        || server == "local"
        || server == "builtin"
        || server == "core"
        || server.is_empty()
        || custom_handlers().contains_key(tool)
        || ToolRegistry::exists(tool);

    let exec_res =
        susi_core::capture::EvidenceSession::capture_call(&call_id, args, workspace, || {
            if let Some(entry) = custom_handlers().get(&call_id) {
                (entry.value())(args, workspace)
            } else if is_native {
                dispatch_native(tool, args, workspace)
            } else {
                GmcpClient::execute_external_tool(server, tool, &args.to_string())
            }
        });

    // 4. Response Formatting
    match exec_res {
        Ok(text) => {
            let is_err = is_tool_result_error(&text);
            Ok(format_tool_response(&text, is_err))
        }
        Err(err) => {
            let err_msg = err.to_string();
            Ok(format_tool_response(&err_msg, true))
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::tempdir;

    #[test]
    fn test_format_tool_response_success_and_error() {
        let ok_val = format_tool_response("success payload", false);
        assert_eq!(ok_val["isError"], false);
        assert_eq!(ok_val["content"][0]["type"], "text");
        assert_eq!(ok_val["content"][0]["text"], "success payload");

        let err_val = format_tool_response("failed payload", true);
        assert_eq!(err_val["isError"], true);
        assert_eq!(err_val["content"][0]["type"], "text");
        assert_eq!(err_val["content"][0]["text"], "failed payload");
    }

    #[test]
    fn test_validate_tool_args_non_object_rejected() {
        let err = validate_tool_args("susi", "status", &json!("not-an-object")).unwrap_err();
        assert!(err.to_string().contains("JSON object"));
    }

    #[test]
    fn test_validate_tool_args_built_in_schemas() {
        // read_file requires "path"
        let ok_args = json!({ "path": "Cargo.toml" });
        assert!(validate_tool_args("susi", "read_file", &ok_args).is_ok());

        let bad_args = json!({ "wrong_key": 123 });
        let err = validate_tool_args("susi", "read_file", &bad_args).unwrap_err();
        assert!(err.to_string().contains("inputSchema"));

        // write_file requires "path" and "content"
        let bad_write = json!({ "path": "file.txt" });
        assert!(validate_tool_args("susi", "write_file", &bad_write).is_err());
        let ok_write = json!({ "path": "file.txt", "content": "hello" });
        assert!(validate_tool_args("susi", "write_file", &ok_write).is_ok());
    }

    #[test]
    fn test_custom_schema_registration_and_enforcement() {
        register_tool_schema(
            "custom_srv",
            "calc",
            json!({
                "type": "object",
                "properties": {
                    "num": { "type": "integer", "minimum": 10 }
                },
                "required": ["num"]
            }),
        );

        let invalid = json!({ "num": 5 });
        assert!(validate_tool_args("custom_srv", "calc", &invalid).is_err());

        let valid = json!({ "num": 42 });
        assert!(validate_tool_args("custom_srv", "calc", &valid).is_ok());
    }

    #[test]
    fn test_call_tool_direct_schema_validation_rejection_formats_error_and_records_evidence() {
        let tmp = tempdir().unwrap();
        let ws = tmp.path();

        // Create an explicit evidence session
        let sess = susi_core::capture::EvidenceSession::new("test-mission", ws, |s| s.to_string())
            .unwrap();
        let _act = susi_core::capture::EvidenceSession::activate(&sess);

        // Call read_file without required path
        let res = call_tool_direct("susi", "read_file", &json!({}), ws).unwrap();
        assert_eq!(res["isError"], true);
        assert!(res["content"][0]["text"]
            .as_str()
            .unwrap()
            .contains("inputSchema"));

        // Evidence ledger should contain the recorded call
        assert!(
            !sess.receipts().is_empty(),
            "Evidence session must record the invalid call"
        );
    }

    #[test]
    fn test_call_tool_direct_native_execution_and_evidence() {
        let tmp = tempdir().unwrap();
        let ws = tmp.path();

        let sess =
            susi_core::capture::EvidenceSession::new("test-exec", ws, |s| s.to_string()).unwrap();
        let _act = susi_core::capture::EvidenceSession::activate(&sess);

        let res = call_tool_direct("susi", "status", &json!({}), ws).unwrap();
        assert_eq!(res["isError"], false);
        let text = res["content"][0]["text"].as_str().unwrap();
        assert!(text.contains("SUSI Engine Version"));

        assert!(!sess.receipts().is_empty());
    }

    #[test]
    fn test_custom_handler_registration_and_execution() {
        let tmp = tempdir().unwrap();
        let ws = tmp.path();

        register_tool_handler(
            "mock_server",
            "greet",
            std::sync::Arc::new(|args, _ws| {
                let name = args.get("name").and_then(|v| v.as_str()).unwrap_or("World");
                Ok(format!("Hello, {name}!"))
            }),
        );

        let res = call_tool_direct(
            "mock_server",
            "greet",
            &json!({ "name": "Antigravity" }),
            ws,
        )
        .unwrap();
        assert_eq!(res["isError"], false);
        assert_eq!(res["content"][0]["text"], "Hello, Antigravity!");
    }

    #[test]
    fn test_read_file_write_file_round_trip_via_call_tool_direct() {
        let tmp = tempdir().unwrap();
        let ws = tmp.path();

        // 1. Write file
        let write_res = call_tool_direct(
            "susi",
            "write_file",
            &json!({ "path": "test.txt", "content": "hello mcp direct" }),
            ws,
        )
        .unwrap();
        assert_eq!(write_res["isError"], false);

        // 2. Read file back
        let read_res =
            call_tool_direct("susi", "read_file", &json!({ "path": "test.txt" }), ws).unwrap();
        assert_eq!(read_res["isError"], false);
        assert_eq!(read_res["content"][0]["text"], "hello mcp direct");
    }
}

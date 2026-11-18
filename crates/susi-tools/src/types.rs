use crate::susi_error::EaiResult;
use std::path::Path;
use std::sync::Arc;

#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct McpTool {
    pub name: String,
    pub description: String,
    /// JSON Schema for a built-in tool's arguments. Dynamic proxies omit it
    /// when the backing server's schema is unavailable.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub input_schema: Option<serde_json::Value>,
}

/// Dynamic Trait for SUSI Substrate Tools
pub trait SusiTool: Send + Sync {
    fn name(&self) -> String;
    fn description(&self) -> String;
    fn execute(&self, arg: &serde_json::Value, workspace: &Path) -> EaiResult<String>;
}

/// Enum representing Meta-Tool Category in SUSI Substrate
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MetaCategory {
    SystemPrimitive,
    WorkspaceIo,
    McpProxy,
    WasmReflex,
    IntelligenceBridge,
    CodingSpecialist,
    AssistantSpecialist,
}

pub type MetaToolHandler =
    Arc<dyn Fn(&serde_json::Value, &Path) -> EaiResult<String> + Send + Sync>;

/// Generic Meta-Tool Struct
pub struct MetaTool {
    pub tool_name: String,
    pub tool_desc: String,
    pub category: MetaCategory,
    pub handler: MetaToolHandler,
}

impl SusiTool for MetaTool {
    fn name(&self) -> String {
        self.tool_name.clone()
    }
    fn description(&self) -> String {
        self.tool_desc.clone()
    }
    fn execute(&self, arg: &serde_json::Value, workspace: &Path) -> EaiResult<String> {
        (self.handler)(arg, workspace)
    }
}

#[cfg(test)]
mod tests {
    use super::{MetaCategory, MetaTool, SusiTool};
    use serde_json::json;
    use std::path::Path;
    use std::sync::Arc;

    #[test]
    fn meta_tool_delegates_to_its_handler() {
        let tool = MetaTool {
            tool_name: "echo_tool".into(),
            tool_desc: "returns the arg".into(),
            category: MetaCategory::SystemPrimitive,
            handler: Arc::new(|arg, _ws| {
                Ok(format!(
                    "echo:{}",
                    arg.get("v").and_then(|v| v.as_i64()).unwrap_or(-1)
                ))
            }),
        };
        assert_eq!(tool.name(), "echo_tool");
        assert_eq!(tool.description(), "returns the arg");
        let out = tool.execute(&json!({"v": 7}), Path::new(".")).unwrap();
        assert_eq!(out, "echo:7");
        let err_tool = MetaTool {
            tool_name: "fail".into(),
            tool_desc: "d".into(),
            category: MetaCategory::McpProxy,
            handler: Arc::new(|_a, _w| Err(crate::susi_error::EaiError::internal("boom"))),
        };
        assert!(err_tool.execute(&json!({}), Path::new(".")).is_err());
    }

    #[test]
    fn mcp_tool_round_trips_through_json() {
        let tool = super::McpTool {
            name: "read_file".into(),
            description: "Read a file — UTF-8 \"quoted\"".into(),
            input_schema: None,
        };
        let value = serde_json::to_value(&tool).unwrap();
        assert_eq!(
            value,
            json!({"name": "read_file", "description": "Read a file — UTF-8 \"quoted\""})
        );
        let back: super::McpTool = serde_json::from_value(value).unwrap();
        assert_eq!(back.name, tool.name);
        assert_eq!(back.description, tool.description);
        assert!(serde_json::from_value::<super::McpTool>(json!({"name": "x"})).is_err());
    }

    #[test]
    fn every_meta_category_is_carried_unchanged_by_a_meta_tool() {
        let all = [
            MetaCategory::SystemPrimitive,
            MetaCategory::WorkspaceIo,
            MetaCategory::McpProxy,
            MetaCategory::WasmReflex,
            MetaCategory::IntelligenceBridge,
            MetaCategory::CodingSpecialist,
            MetaCategory::AssistantSpecialist,
        ];
        for (i, category) in all.into_iter().enumerate() {
            let tool = MetaTool {
                tool_name: format!("t{i}"),
                tool_desc: String::new(),
                category,
                handler: Arc::new(|_a, _w| Ok(String::new())),
            };
            assert_eq!(tool.category, category);
            assert_eq!(all.iter().filter(|c| **c == tool.category).count(), 1);
        }
    }

    #[test]
    fn execute_forwards_the_exact_workspace_and_argument() {
        let workspace = Path::new("C:/susi/workspace with space/sub");
        let tool = MetaTool {
            tool_name: "ws_echo".into(),
            tool_desc: String::new(),
            category: MetaCategory::WorkspaceIo,
            handler: Arc::new(|arg, ws| Ok(format!("{}|{}", ws.display(), arg))),
        };
        let out = tool.execute(&json!({"k": [1, 2]}), workspace).unwrap();
        assert_eq!(
            out,
            format!("{}|{}", workspace.display(), json!({"k": [1, 2]}))
        );
    }

    #[test]
    fn mcp_tool_clone_and_debug_preserve_name_and_description() {
        let tool = super::McpTool {
            name: "list_dir".into(),
            description: "List directory contents".into(),
            input_schema: None,
        };
        let cloned = tool.clone();
        assert_eq!(cloned.name, "list_dir");
        assert_eq!(cloned.description, "List directory contents");
        assert!(format!("{cloned:?}").contains("list_dir"));
    }

    #[test]
    fn mcp_tool_preserves_a_typed_input_schema_when_present() {
        let tool = super::McpTool {
            name: "write_file".into(),
            description: "Write a file".into(),
            input_schema: Some(json!({
                "type": "object",
                "properties": {"path": {"type": "string"}},
                "required": ["path"]
            })),
        };
        let encoded = serde_json::to_value(&tool).unwrap();
        assert_eq!(encoded["input_schema"]["required"], json!(["path"]));
        let decoded: super::McpTool = serde_json::from_value(encoded).unwrap();
        assert_eq!(decoded.input_schema, tool.input_schema);
    }
}

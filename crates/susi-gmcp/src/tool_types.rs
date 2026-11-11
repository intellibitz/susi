use crate::susi_error::EaiResult;
use serde::{Deserialize, Serialize};
use std::path::Path;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct McpTool {
    pub name: String,
    pub description: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub input_schema: Option<serde_json::Value>,
}

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

pub trait SusiTool: Send + Sync {
    fn name(&self) -> String;
    fn description(&self) -> String;
    fn execute(&self, arg: &serde_json::Value, workspace: &Path) -> EaiResult<String>;
}

#[cfg(test)]
mod inline_tests {
    use super::*;
    use serde_json::json;

    struct DummySusiTool;

    impl SusiTool for DummySusiTool {
        fn name(&self) -> String {
            "dummy_tool".into()
        }

        fn description(&self) -> String {
            "A deterministic test tool".into()
        }

        fn execute(&self, arg: &serde_json::Value, workspace: &Path) -> EaiResult<String> {
            Ok(format!(
                "{}:{}",
                workspace.file_name().and_then(|s| s.to_str()).unwrap_or(""),
                arg
            ))
        }
    }

    #[test]
    fn mcp_tool_round_trips_through_serde_json() {
        let tool = McpTool {
            name: "read_file".into(),
            description: "Read a file from disk".into(),
            input_schema: None,
        };
        let encoded = serde_json::to_string(&tool).unwrap();
        let decoded: McpTool = serde_json::from_str(&encoded).unwrap();
        assert_eq!(decoded.name, "read_file");
        assert_eq!(decoded.description, "Read a file from disk");
    }

    #[test]
    fn mcp_tool_supports_clone_and_debug_formatting() {
        let tool = McpTool {
            name: "search".into(),
            description: "Search workspace".into(),
            input_schema: None,
        };
        let cloned = tool.clone();
        assert_eq!(cloned.name, tool.name);
        assert!(format!("{cloned:?}").contains("search"));
    }

    #[test]
    fn meta_category_variants_are_distinct_and_copyable() {
        let categories = [
            MetaCategory::SystemPrimitive,
            MetaCategory::WorkspaceIo,
            MetaCategory::McpProxy,
            MetaCategory::WasmReflex,
            MetaCategory::IntelligenceBridge,
            MetaCategory::CodingSpecialist,
            MetaCategory::AssistantSpecialist,
        ];
        for (i, a) in categories.iter().enumerate() {
            for (j, b) in categories.iter().enumerate() {
                assert_eq!(i == j, a == b);
            }
        }
    }

    #[test]
    fn susi_tool_trait_dispatches_name_description_and_execute() {
        let tool: Box<dyn SusiTool> = Box::new(DummySusiTool);
        assert_eq!(tool.name(), "dummy_tool");
        assert_eq!(tool.description(), "A deterministic test tool");
        let out = tool.execute(&json!(7), Path::new("/tmp/ws")).unwrap();
        assert_eq!(out, "ws:7");
    }

    #[test]
    fn meta_category_debug_formatting_identifies_all_seven_variants() {
        let labels: Vec<String> = [
            MetaCategory::SystemPrimitive,
            MetaCategory::WorkspaceIo,
            MetaCategory::McpProxy,
            MetaCategory::WasmReflex,
            MetaCategory::IntelligenceBridge,
            MetaCategory::CodingSpecialist,
            MetaCategory::AssistantSpecialist,
        ]
        .into_iter()
        .map(|c| format!("{c:?}"))
        .collect();
        assert_eq!(labels.len(), 7);
        assert!(labels.contains(&"WasmReflex".to_string()));
        assert!(labels.contains(&"IntelligenceBridge".to_string()));
    }
}

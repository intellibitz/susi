use crate::susi_error::EaiResult;
use serde::{Deserialize, Serialize};
use std::path::Path;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct McpTool {
    pub name: String,
    pub description: String,
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

// ---- Wave 13 (T-CLAUDE-412) structural invariant verifiers ----

impl McpTool {
    /// A tool the core may advertise: a non-empty name that is a safe MCP identifier
    /// (ASCII letters, digits, `_`, `-`, `.`, with no whitespace), and a non-empty
    /// description so a caller can tell what it does.
    pub fn verify_tool_invariants(&self) -> Result<(), String> {
        if self.name.trim().is_empty() {
            return Err("McpTool name must not be empty".to_string());
        }
        if self
            .name
            .chars()
            .any(|c| !(c.is_ascii_alphanumeric() || matches!(c, '_' | '-' | '.')))
        {
            return Err(format!(
                "McpTool name `{}` has characters outside [A-Za-z0-9_.-]",
                self.name
            ));
        }
        if self.description.trim().is_empty() {
            return Err("McpTool description must not be empty".to_string());
        }
        Ok(())
    }
}

#[cfg(test)]
mod wave13_verifier_tests {
    use super::*;

    #[test]
    fn mcp_tool_verifier_rejects_bad_names_and_empty_descriptions() {
        let ok = McpTool {
            name: "workspace.read_file".to_string(),
            description: "Read a file from the workspace".to_string(),
        };
        assert_eq!(ok.verify_tool_invariants(), Ok(()));

        let mut spaced = ok.clone();
        spaced.name = "read file".to_string();
        assert!(spaced.verify_tool_invariants().is_err());

        let mut empty_name = ok.clone();
        empty_name.name = "  ".to_string();
        assert!(empty_name.verify_tool_invariants().is_err());

        let mut no_desc = ok;
        no_desc.description = String::new();
        assert!(no_desc.verify_tool_invariants().is_err());
    }
}

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
#[allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
#[path = "tests/tool_types.rs"]
mod inline_tests;

use serde::{Deserialize, Serialize};
use std::path::Path;
use susi_error::EaiResult;

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

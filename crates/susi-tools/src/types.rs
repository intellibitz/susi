use crate::susi_error::EaiResult;
use std::path::Path;
use std::sync::Arc;

#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct McpTool {
    pub name: String,
    pub description: String,
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

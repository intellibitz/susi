//! Provider command implementation is served by Susi Commercial Apps.
use crate::anyhow::Result;
pub use susi_cli_defs::model::ModelCommands;

pub fn execute(_action: Option<ModelCommands>, workspace: &std::path::Path) -> Result<()> {
    super::provider_forward::execute(Some(workspace))
}

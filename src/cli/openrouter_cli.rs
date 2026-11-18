//! Provider command implementation is served by Susi Commercial Apps.
use crate::anyhow::Result;
pub use susi_cli_defs::openrouter::OpenRouterCommands;

pub fn execute(_action: Option<OpenRouterCommands>) -> Result<()> {
    super::provider_forward::execute(None)
}

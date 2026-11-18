//! Provider command implementation is served by Susi Commercial Apps.
use crate::anyhow::Result;
pub use susi_cli_defs::frontier::FrontierCommands;

pub fn execute(_action: Option<FrontierCommands>) -> Result<()> {
    super::provider_forward::execute(None)
}

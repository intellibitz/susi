//! Provider command implementation is served by Susi Commercial Apps.
use crate::anyhow::Result;
pub use susi_cli_defs::brain::BrainCommands;

pub fn execute(_action: Option<BrainCommands>) -> Result<()> {
    super::provider_forward::execute(None)
}

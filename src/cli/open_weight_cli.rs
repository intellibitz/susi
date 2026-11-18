//! Provider command implementation is served by Susi Commercial Apps.
use crate::anyhow::Result;
pub use susi_cli_defs::open_weight::OpenWeightCommands;

pub fn execute(_action: Option<OpenWeightCommands>) -> Result<()> {
    super::provider_forward::execute(None)
}

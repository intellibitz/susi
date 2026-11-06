//! `clap` definitions for the `blackboard` command family, moved verbatim from
//! `src/cli/blackboard_cli.rs` (D-SP-4 Stage 4c). The handlers stay in the root
//! crate, which re-exports these types from `src/cli/blackboard_cli.rs`.
use clap::Subcommand;

/// Subcommands of `susi blackboard`: inspect the last mission blackboard.
#[derive(Debug, Subcommand)]
pub enum BlackboardCommands {
    /// Show `.susi/last_blackboard.json` from the current workspace (default)
    Show,
}

//! `clap` definitions for the `blackboard` command family, moved verbatim from
//! `src/cli/blackboard_cli.rs` (D-SP-4 Stage 4c). The handlers stay in the root
//! crate, which re-exports these types from `src/cli/blackboard_cli.rs`.
use clap::Subcommand;

/// Subcommands of `susi blackboard`: inspect or post to the mission blackboard
/// (posts always mirror onto the Susi Native Board).
#[derive(Debug, Subcommand)]
pub enum BlackboardCommands {
    /// Show `.susi/last_blackboard.json` from the current workspace (default)
    Show,
    /// Post a key/value entry to the shared blackboard and Susi Native Board
    Post {
        /// Key to write
        key: String,
        /// JSON string or plain text value
        value: String,
        /// Authoring agent (defaults to SUSI_AGENT or git config susi.agent)
        #[arg(long)]
        author: Option<String>,
        /// Associated task identifier
        #[arg(long)]
        task: Option<String>,
        /// Board item kind when mirroring (default: note)
        #[arg(long)]
        kind: Option<String>,
        /// Board item title override when mirroring
        #[arg(long)]
        title: Option<String>,
    },
}

//! `clap` definitions for the `setup` command family, moved verbatim from
//! `src/cli/setup_cli.rs` (D-SP-4 Stage 4d). The handlers stay in the root
//! crate, which re-exports these types from `src/cli/setup_cli.rs`.
//!
//! An item marked `#[allow(missing_docs)]` keeps undocumented fields (or is
//! hidden) on purpose: clap turns every `///` comment into `--help` text, and
//! this move must leave the visible help byte-identical.
use clap::Subcommand;

/// Subcommands of `susi setup`: zero-config ecosystem setup.
#[derive(Debug, Subcommand)]
pub enum SetupCommands {
    /// Detect hardware/engines/keys/agents and print the plan (default)
    Plan {
        /// Machine-readable plan for automation
        #[arg(long)]
        json: bool,
    },
    /// Apply the plan (optionally grant cloud consent)
    Apply {
        /// Grant cloud API key consent in this apply
        #[arg(long)]
        cloud: bool,
        /// Machine-readable result for automation
        #[arg(long)]
        json: bool,
    },
}

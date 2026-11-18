//! Native `clap` definitions for the public `setup` command family.
//! The handlers live in `susi-commercial-apps/crates/susi-cli-runtime`,
//! which re-exports these types and receives the original public arguments.
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
